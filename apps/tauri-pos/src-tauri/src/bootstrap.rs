//! Bringing the kiosk up on a machine nobody has configured.
//!
//! On a developer's Mac the three pieces are started by hand. On an installed
//! Windows kiosk nobody is going to open a terminal, so this module does the
//! same three things in the same order and keeps the result honest:
//!
//! ```text
//! 1. decide where mutable state lives      (never inside Program Files)
//! 2. start PI.Edge, or adopt the running one
//! 3. seed the demo once, and never again
//! ```
//!
//! **No second core.** Both steps run the very binaries the development flow
//! runs — `edge-daemon` and `edge-cli`, shipped beside the app — so the
//! installed kiosk and the developer's kiosk are the same runtime, started
//! differently.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long to wait for the core to answer before giving up on it.
const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// How often to ask, while waiting.
const READY_POLL: Duration = Duration::from_millis(120);

/// Windows: don't flash a console window when spawning a companion process.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug)]
pub enum BootstrapError {
    /// The app-data directory could not be created.
    DataDirectory(String),
    /// A companion binary is not beside the app.
    MissingBinary(PathBuf),
    /// The core did not come up.
    CoreUnavailable(String),
    /// The demo could not be installed.
    Seed(String),
}

impl std::error::Error for BootstrapError {}

impl std::fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DataDirectory(detail) => {
                write!(f, "no writable data directory for PX Kiosk: {detail}")
            }
            Self::MissingBinary(path) => {
                write!(f, "this install is missing {}", path.display())
            }
            Self::CoreUnavailable(detail) => write!(f, "PI.Edge did not start: {detail}"),
            Self::Seed(detail) => write!(f, "the demo could not be installed: {detail}"),
        }
    }
}

/// The daemon this process started, if it started one.
///
/// Held so the window can stop it on the way out. A daemon that was *already*
/// running is somebody else's — adopted for the session, never killed.
#[derive(Debug, Default)]
pub struct CoreProcess(Mutex<Option<Child>>);

impl CoreProcess {
    /// Stop the daemon this process owns. Idempotent, and silent about one it
    /// does not own.
    pub fn shutdown(&self) {
        let Ok(mut guard) = self.0.lock() else {
            return;
        };
        let Some(mut child) = guard.take() else {
            return;
        };
        tracing::info!(pid = child.id(), "stopping the PI.Edge this window started");
        // The daemon releases its single-writer lock on exit; killing it is
        // the same path a Ctrl-C takes.
        let _ = child.kill();
        let _ = child.wait();
    }

    fn adopt(&self, child: Child) {
        if let Ok(mut guard) = self.0.lock() {
            *guard = Some(child);
        }
    }
}

/// Where mutable state lives.
///
/// Never the install directory: on Windows that is Program Files, which is
/// read-only for the user running the kiosk, and an installer that writes a
/// database there produces an app that works for the person who installed it
/// and fails for everybody else. Tauri resolves the platform's own answer —
/// `%LOCALAPPDATA%\PX Kiosk` on Windows — so nothing here hardcodes a path.
pub fn data_dir(app: &tauri::AppHandle) -> Result<PathBuf, BootstrapError> {
    use tauri::Manager;
    let dir = app
        .path()
        .app_local_data_dir()
        .map_err(|error| BootstrapError::DataDirectory(error.to_string()))?;
    std::fs::create_dir_all(&dir)
        .map_err(|error| BootstrapError::DataDirectory(format!("{}: {error}", dir.display())))?;
    Ok(dir)
}

/// A companion binary shipped beside the app.
///
/// Tauri puts `externalBin` entries next to the executable with the target
/// triple stripped, so this looks for the plain name first and falls back to
/// the development layout.
fn companion(name: &str) -> Result<PathBuf, BootstrapError> {
    let exe = std::env::current_exe()
        .map_err(|error| BootstrapError::DataDirectory(error.to_string()))?;
    let dir = exe.parent().unwrap_or(Path::new("."));
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let beside = dir.join(format!("{name}{suffix}"));
    if beside.is_file() {
        return Ok(beside);
    }
    // Development: the workspace target directory, three levels up from
    // `apps/tauri-pos/src-tauri`.
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/debug")
        .join(format!("{name}{suffix}"));
    if dev.is_file() {
        return Ok(dev);
    }
    Err(BootstrapError::MissingBinary(beside))
}

fn configure(command: &mut Command) {
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
}

/// Is the core answering on this installation?
fn core_is_up(database: &Path) -> bool {
    edge_client::IpcEdgeClient::for_database(database, "px-kiosk")
        .map(|client| {
            use edge_client::EdgeClient;
            client.core_info().is_ok()
        })
        .unwrap_or(false)
}

/// Start PI.Edge, or adopt the one already serving this installation.
///
/// Two kiosks over one database would be two writers, and the daemon's own
/// single-writer lock already refuses that. So the rule here is: ask first,
/// start only if nobody answered, and never kill what we did not start.
pub fn ensure_core(database: &Path, owned: &CoreProcess) -> Result<(), BootstrapError> {
    if core_is_up(database) {
        tracing::info!("PI.Edge is already serving this installation; adopting it");
        return Ok(());
    }

    let daemon = companion("edge-daemon")?;
    tracing::info!(binary = %daemon.display(), "starting PI.Edge");
    let mut command = Command::new(&daemon);
    command.arg("--db").arg(database);
    configure(&mut command);
    let child = command
        .spawn()
        .map_err(|error| BootstrapError::CoreUnavailable(error.to_string()))?;
    owned.adopt(child);

    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        if core_is_up(database) {
            return Ok(());
        }
        std::thread::sleep(READY_POLL);
    }
    Err(BootstrapError::CoreUnavailable(format!(
        "it did not answer within {}s",
        READY_TIMEOUT.as_secs()
    )))
}

/// Does this installation already have a menu?
///
/// The question that makes bootstrap idempotent. A second launch must not
/// import the catalog again — that would stack a new snapshot on every start
/// — so the core is asked whether one is already active, and the answer is
/// the core's, not a marker file somebody could delete.
fn already_seeded(database: &Path) -> bool {
    use edge_client::EdgeClient;
    edge_client::IpcEdgeClient::for_database(database, "px-kiosk")
        .ok()
        .and_then(|client| client.catalog_status().ok())
        .map(|status| status.active.is_some())
        .unwrap_or(false)
}

/// Install the bundled demo, once.
///
/// Runs the same `demo-seed` the development flow runs, against the demo that
/// shipped inside the installer. Returns `false` when there was nothing to do.
pub fn seed_once(database: &Path, demo: &Path) -> Result<bool, BootstrapError> {
    if already_seeded(database) {
        tracing::info!("this installation already has an active menu; not re-seeding");
        return Ok(false);
    }
    if !demo.join("menu.json").is_file() {
        return Err(BootstrapError::Seed(format!(
            "the bundled demo is missing from {}",
            demo.display()
        )));
    }

    let cli = companion("edge-cli")?;
    tracing::info!(demo = %demo.display(), "installing the bundled demo");
    let mut command = Command::new(&cli);
    command
        .arg("--db")
        .arg(database)
        .arg("--format")
        .arg("json")
        .arg("demo-seed")
        .arg("--path")
        .arg(demo)
        .env("PI_EDGE_LOG", "warn");
    configure(&mut command);
    // Captured rather than discarded: if this fails it is the one thing the
    // log needs to say.
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let output = command
        .output()
        .map_err(|error| BootstrapError::Seed(error.to_string()))?;
    if !output.status.success() {
        return Err(BootstrapError::Seed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    tracing::info!("the bundled demo is installed");
    Ok(true)
}

/// Where the bundled demo lives in an installed app.
pub fn bundled_demo(app: &tauri::AppHandle) -> PathBuf {
    use tauri::Manager;
    app.path()
        .resolve("demo", tauri::path::BaseDirectory::Resource)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../demo"))
}
