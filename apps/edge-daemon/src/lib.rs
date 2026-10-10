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
pub mod hardware_config;
pub mod refund_authority;
pub mod state;
pub mod sync_config;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use edge_application::ports::Clock;
use edge_application::sync_service::SyncRuntime;
use edge_application::EdgeApplication;
use edge_application::{SystemClock, UuidFactory};
use edge_ipc::credentials::{FileCredentialProvider, LocalCredentialProvider};
use edge_ipc::{DesktopLocalListener, EdgeListener, RuntimePaths};
use edge_payments::{SimulatorPaymentProvider, SimulatorScript};
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

/// How often the runtime tries to drain the outbox.
///
/// Only runs at all when a transport is configured. Facts that miss a tick
/// lose nothing: they are durable, and the next tick takes them.
pub const DEFAULT_SYNC_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Database path; the runtime layout and endpoint derive from it.
    pub database: PathBuf,
    pub accept_poll: Duration,
    pub shutdown_grace: Duration,
    pub sync_interval: Duration,
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
            sync_interval: DEFAULT_SYNC_INTERVAL,
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
    // There is no real payment provider yet, so the runtime settles cards
    // through the deterministic simulator. `PI_EDGE_PAYMENT_SIM` scripts it
    // for development and for the end-to-end demo; reading it here, in the
    // composition root, keeps the domain free of environment lookups.
    let provider = payment_provider()?;
    // Which devices this installation has. Read here and nowhere else: a host
    // or a port must never be reachable from a use case or a protocol payload
    // (ADR 039).
    let hardware = hardware_config::from_env().map_err(DaemonError::Configuration)?;
    // Whether there is a server to talk to. Absent means no: facts accumulate
    // durably and the till keeps selling (ADR 040).
    let transport = sync_config::from_env().map_err(DaemonError::Configuration)?;
    let sync = match &transport {
        Some(transport) => SyncRuntime::with_transport(Arc::clone(transport)),
        None => SyncRuntime::unconfigured(),
    };
    let core = Arc::new(
        EdgeApplication::with_hardware(store, SystemClock, UuidFactory, provider, hardware)
            .with_sync(sync),
    );
    // How refunds get authority over an original sale. The explicit
    // development switch wins when it is set; otherwise a configured
    // transport *is* the authority, and with neither every refund is refused.
    // The production rule — a refund is authorised online, for that one
    // operation — is the default in all three cases (ADR 030, ADR 042).
    let refund_authority = match refund_authority::RefundAuthoritySource::from_env()
        .map_err(DaemonError::Configuration)?
    {
        refund_authority::RefundAuthoritySource::Unavailable => match &transport {
            Some(transport) => {
                refund_authority::RefundAuthoritySource::Transport(Arc::clone(transport))
            }
            None => refund_authority::RefundAuthoritySource::Unavailable,
        },
        explicit => explicit,
    };

    // Anything left in flight when the previous process stopped goes back in
    // the queue, with the same event ids. Done whether or not a transport is
    // configured: it is local state, and a `SENDING` row is never a resting
    // state (ADR 041).
    core.recover_interrupted_sync()?;
    let state = Arc::new(state::DaemonState::new(migrations::target_version()));

    let endpoint = paths.endpoint()?;
    let listener = DesktopLocalListener::bind(endpoint.clone())?;
    lock.write_diagnostics(
        std::process::id(),
        endpoint.label(),
        &SystemClock.now().to_rfc3339(),
    )?;

    let dispatcher = Arc::new(Dispatcher::with_refund_authority(
        Arc::clone(&core),
        Arc::clone(&state),
        refund_authority,
    ));
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

    // The dispatcher, when there is somewhere to dispatch to. One thread,
    // one owner of the outbox — which is exactly why recovery needs no lease
    // and no heartbeat (ADR 041).
    if transport.is_some() {
        let core = Arc::clone(&core);
        let shutdown = Arc::clone(&shutdown);
        let interval = config.sync_interval;
        std::thread::Builder::new()
            .name("edge-sync".to_owned())
            .spawn(move || sync_loop(&core, &shutdown, interval))?;
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

/// Drain the outbox, over and over, until shutdown.
///
/// A cycle that finds the server unreachable is not an error worth escalating:
/// the facts are durable, the backoff is recorded, and the next tick tries
/// again. Only a storage fault is logged as a failure, because that is a bug.
fn sync_loop(core: &Arc<dispatch::Core>, shutdown: &Arc<AtomicBool>, interval: Duration) {
    while !shutdown.load(Ordering::SeqCst) {
        match core.run_sync_cycle() {
            Ok(report) => {
                if report.attempted > 0 || report.inbound_applied > 0 {
                    tracing::info!(
                        attempted = report.attempted,
                        acknowledged = report.acknowledged,
                        rescheduled = report.rescheduled,
                        failed = report.failed,
                        inbound_applied = report.inbound_applied,
                        "sync cycle"
                    );
                }
            }
            Err(error) => tracing::error!(%error, "a sync cycle could not complete"),
        }
        // Slept in slices so a signal does not wait out the whole interval.
        let deadline = Instant::now() + interval;
        while Instant::now() < deadline && !shutdown.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
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

/// Build the payment provider from the environment.
///
/// Development-only switch. An unparseable script is a hard startup failure
/// rather than a silent fallback: a till that behaves differently from what an
/// operator configured is worse than one that refuses to start.
fn payment_provider() -> Result<SimulatorPaymentProvider, DaemonError> {
    let Some(spec) = std::env::var_os(PAYMENT_SIM_ENV) else {
        return Ok(SimulatorPaymentProvider::approving());
    };
    let spec = spec.to_string_lossy().to_string();
    let script = SimulatorScript::parse(&spec)
        .map_err(|error| DaemonError::Configuration(error.to_string()))?;
    tracing::warn!(
        script = %spec,
        "payment provider is a scripted simulator; not for production use"
    );
    Ok(SimulatorPaymentProvider::new(script))
}

/// Name of the development switch that scripts the payment simulator.
pub const PAYMENT_SIM_ENV: &str = "PI_EDGE_PAYMENT_SIM";
