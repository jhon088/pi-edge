//! `edge-daemon` — the PI.Edge production runtime.
//!
//! The daemon is the process that *is* the POS. It owns the single-writer
//! lock, the SQLite database and the application core, and it serves local
//! clients over the Edge Protocol. UIs come and go; the register does not.
//!
//! Startup order matters and is enforced here:
//!
//! 1. resolve the runtime layout from the database path,
//! 2. create the runtime directory with owner-only permissions,
//! 3. **acquire the single-writer lock before touching the database** — if
//!    another runtime owns the installation this process must not open the
//!    files at all,
//! 4. load or provision the local credential,
//! 5. open and migrate the database,
//! 6. bind the IPC endpoint,
//! 7. announce readiness,
//! 8. serve until a signal arrives, then drain and release.

pub mod connection;
pub mod dispatch;
pub mod error;
pub mod state;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use edge_application::adapters::{SystemClock, UuidFactory};
use edge_application::ports::Clock;
use edge_application::EdgeApplication;
use edge_ipc::credentials::{FileCredentialProvider, LocalCredentialProvider};
use edge_ipc::{DesktopLocalListener, EdgeListener, RuntimePaths};
use edge_protocol::{CoreStatusV1, PROTOCOL_VERSION};
use edge_storage::{migrations, SingleWriterLock, SqliteEdgeStore};

use crate::connection::ConnectionContext;
use crate::dispatch::Dispatcher;
use crate::error::DaemonError;

pub use crate::error::EXIT_SINGLE_WRITER;

/// Version of the daemon binary.
pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How often the accept loop checks the shutdown flag. Short enough that
/// `SIGTERM` feels immediate, long enough to stay invisible in a profile.
pub const DEFAULT_ACCEPT_POLL: Duration = Duration::from_millis(20);

/// How long shutdown waits for in-flight requests to finish.
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Database path; the runtime layout and endpoint derive from it.
    pub database: PathBuf,
    pub accept_poll: Duration,
    pub shutdown_grace: Duration,
    /// Print one machine-readable readiness line on stdout, for supervisors
    /// and tests. Never contains the credential.
    pub announce_ready: bool,
}

impl DaemonConfig {
    pub fn new(database: impl Into<PathBuf>) -> Self {
        Self {
            database: database.into(),
            accept_poll: DEFAULT_ACCEPT_POLL,
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
            announce_ready: true,
        }
    }
}

/// Run until `SIGINT`/`SIGTERM`.
pub fn run(config: DaemonConfig) -> Result<(), DaemonError> {
    let shutdown = Arc::new(AtomicBool::new(false));
    register_signals(&shutdown)?;
    run_until(config, shutdown)
}

/// Run until the flag flips. Exposed so tests can drive the lifecycle without
/// sending signals.
pub fn run_until(config: DaemonConfig, shutdown: Arc<AtomicBool>) -> Result<(), DaemonError> {
    let paths = RuntimePaths::for_database(&config.database)?;
    paths.ensure_runtime_dir()?;

    // Runtime identity first: never open the database if someone else owns it.
    let mut lock = SingleWriterLock::acquire(paths.lock_file())?;

    let credentials = FileCredentialProvider::new(paths.token_file());
    let token = credentials.load_or_create()?;

    let store = SqliteEdgeStore::open(paths.database())?;
    let core = Arc::new(EdgeApplication::new(store, SystemClock, UuidFactory));
    let state = Arc::new(state::DaemonState::new(migrations::target_version()));

    let endpoint = paths.endpoint()?;
    let listener = DesktopLocalListener::bind(endpoint.clone())?;
    lock.write_diagnostics(
        std::process::id(),
        endpoint.label(),
        &SystemClock.now().to_rfc3339(),
    )?;

    let dispatcher = Arc::new(Dispatcher::new(Arc::clone(&core), Arc::clone(&state)));
    let context = Arc::new(ConnectionContext::new(Arc::clone(&dispatcher), token));

    state.set_status(CoreStatusV1::Ready);
    tracing::info!(
        database = %paths.database().display(),
        endpoint = %endpoint.label(),
        schema_version = state.schema_version(),
        protocol_version = PROTOCOL_VERSION,
        pid = std::process::id(),
        "edge daemon ready"
    );
    if config.announce_ready {
        announce_ready(endpoint.label(), state.schema_version());
    }

    // Accepting blocks, so it lives on its own thread and hands connections
    // to the main loop, which stays free to notice a shutdown signal.
    let listener = Arc::new(listener);
    let (incoming, accepted) = std::sync::mpsc::channel();
    {
        let listener = Arc::clone(&listener);
        std::thread::Builder::new()
            .name("edge-accept".to_owned())
            .spawn(move || accept_loop(&listener, &incoming))?;
    }

    while !shutdown.load(Ordering::SeqCst) {
        match accepted.recv_timeout(config.accept_poll) {
            Ok(connection) => {
                let context = Arc::clone(&context);
                std::thread::Builder::new()
                    .name("edge-connection".to_owned())
                    .spawn(move || connection::serve(connection, context))?;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                tracing::error!("the accept loop stopped; shutting down");
                break;
            }
        }
    }

    shutdown_gracefully(&state, &listener, config.shutdown_grace);
    drop(lock);
    tracing::info!("edge daemon stopped");
    Ok(())
}

/// Hand every accepted connection to the main loop until the endpoint closes.
fn accept_loop(
    listener: &DesktopLocalListener,
    incoming: &std::sync::mpsc::Sender<Box<dyn edge_ipc::ServerConnection>>,
) {
    loop {
        match listener.accept() {
            Ok(Some(connection)) => {
                if incoming.send(connection).is_err() {
                    return;
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => {
                // Normal at shutdown: the endpoint is gone.
                tracing::debug!(%error, "accept loop ending");
                return;
            }
        }
    }
}

/// Stop accepting, let in-flight work finish, release everything.
fn shutdown_gracefully(
    state: &Arc<state::DaemonState>,
    listener: &DesktopLocalListener,
    grace: Duration,
) {
    state.set_status(CoreStatusV1::Stopping);
    // Removing the endpoint first means no client can start new work while we
    // are draining.
    listener.shutdown();

    let deadline = Instant::now() + grace;
    while state.in_flight() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    if state.in_flight() > 0 {
        tracing::warn!(
            in_flight = state.in_flight(),
            "shutdown grace expired with requests still running"
        );
    }
    // Dropping every sender wakes the parked event-stream threads.
    state.clear_subscribers();
}

fn announce_ready(endpoint: &str, schema_version: i64) {
    let line = serde_json::json!({
        "status": CoreStatusV1::Ready.as_str(),
        "endpoint": endpoint,
        "pid": std::process::id(),
        "core_version": edge_application::CORE_VERSION,
        "daemon_version": DAEMON_VERSION,
        "protocol_version": PROTOCOL_VERSION,
        "database_schema_version": schema_version,
    });
    println!("{line}");
    // A supervisor may be reading this line to know when to start the UI.
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

fn register_signals(shutdown: &Arc<AtomicBool>) -> Result<(), DaemonError> {
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(shutdown))?;
    #[cfg(unix)]
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(shutdown))?;
    Ok(())
}
