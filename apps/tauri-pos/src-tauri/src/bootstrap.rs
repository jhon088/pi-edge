//! Bringing the kiosk up on a machine nobody has configured.
//!
//! On a developer's Mac the three pieces are started by hand. On an installed
//! Windows kiosk nobody is going to open a terminal, so this module does the
//! same three things in the same order and keeps the result honest:
//!
//! ```text
//! 1. decide where mutable state lives      (never inside Program Files)
//! 2. start PI.Edge, or adopt the running one
//! 3. install the bundled demo unless every lane already sells
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
/// and fails for everybody else. Tauri resolves the platform's own answer,
/// named after the identifier — `%LOCALAPPDATA%\com.pxcommerce.pxkiosk` on
/// Windows — so nothing here hardcodes a path.
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

/// Which of this build's lanes this installation cannot serve.
///
/// The question `ensure_demo` actually has to answer, and **not** the one it
/// used to ask. "Is a catalog active?" is a different question, and the
/// difference is the bug: an installation seeded by the release *before* Tim
/// Hortons and Sbarro existed has a perfectly active catalog and two blank
/// lanes. Every launch answered "already seeded" and skipped the import that
/// would have filled them, so on an upgraded Windows kiosk both brands
/// appeared in Admin — the shell lists them from its own table — and showed
/// no products and no photographs. A clean install was fine, which is why it
/// survived CI: the installer stages every brand's dataset correctly, and
/// nothing ever imported two of them.
///
/// The answer is still the core's rather than a marker file somebody could
/// delete: four menu reads, one per lane this build ships.
fn lanes_without_a_menu(database: &Path) -> Vec<&'static str> {
    use edge_client::EdgeClient;
    let every = || -> Vec<&'static str> {
        crate::showroom::Vertical::ALL
            .iter()
            .map(|lane| lane.channel())
            .collect()
    };

    let Ok(client) = edge_client::IpcEdgeClient::for_database(database, "px-kiosk") else {
        return every();
    };
    // Nothing imported at all: every lane is blank and there is no point
    // asking four times.
    match client.catalog_status() {
        Ok(status) if status.active.is_some() => {}
        // Either no catalog, or a core that cannot say. Both are answered by
        // seeding, and a seed that cannot reach the core reports the real
        // reason rather than this guess.
        _ => return every(),
    }

    crate::showroom::Vertical::ALL
        .iter()
        .map(|lane| lane.channel())
        .filter(|channel| {
            client
                .menu(Some((*channel).to_owned()))
                .map(|menu| menu.items.is_empty())
                .unwrap_or(true)
        })
        .collect()
}

/// Install the bundled demo unless every lane this build ships already sells.
///
/// Runs the same `demo-seed` the development flow runs, against the demo that
/// shipped inside the installer. That command is idempotent and upgrade-safe
/// — it reconciles the catalog version against what the core already holds,
/// and a dataset that has not changed produces no second snapshot — so the
/// guard here only has to decide whether there is anything to install, and
/// may err towards running it. Returns `false` when there was nothing to do.
pub fn ensure_demo(
    database: &Path,
    demo: &Path,
    overrides: Option<&Path>,
) -> Result<bool, BootstrapError> {
    let blank = lanes_without_a_menu(database);
    if blank.is_empty() {
        tracing::info!("every lane this build ships already has its menu; not re-seeding");
        return Ok(false);
    }
    tracing::info!(
        lanes = blank.join(", "),
        "these lanes have no menu; installing the bundled demo"
    );
    reseed(database, demo, overrides).map(|()| true)
}

/// Import the bundled demo now, whatever is already in force.
///
/// What a price change runs. An operator edited a number, so the catalog has
/// to carry it — and the only way a number becomes the one a sale charges is
/// a new active snapshot, built by the same pipeline a release uses (ADR 050).
///
/// Unconditional on purpose: the guard above asks whether a *lane* is blank,
/// which a changed price is not. Safe to call repeatedly — `demo-seed`
/// reconciles the version against what the core holds and an unchanged
/// dataset produces no second snapshot.
pub fn reseed(
    database: &Path,
    demo: &Path,
    overrides: Option<&Path>,
) -> Result<(), BootstrapError> {
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
        .arg(demo);
    // Prices an operator changed on this machine, applied to the dataset
    // before it is imported. A missing file is nothing overridden, which the
    // CLI handles, so the flag is passed whenever a path is known rather than
    // only when the file exists.
    if let Some(overrides) = overrides {
        command.arg("--overrides").arg(overrides);
    }
    command.env("PI_EDGE_LOG", "warn");
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
    tracing::info!(
        report = %String::from_utf8_lossy(&output.stdout).trim(),
        "the bundled demo is installed"
    );
    Ok(())
}

/// Where the bundled demo lives in an installed app.
pub fn bundled_demo(app: &tauri::AppHandle) -> PathBuf {
    use tauri::Manager;
    app.path()
        .resolve("demo", tauri::path::BaseDirectory::Resource)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../demo"))
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    /// The dataset that ships inside the installer, as the checkout holds it.
    fn demo_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../demo")
    }

    /// The dataset as the release *before* Tim Hortons and Sbarro shipped it:
    /// the restaurant alone, with no brand folders beside it and therefore no
    /// availability rows naming aisles it was never given.
    ///
    /// This is what an upgraded Windows kiosk actually has in its database,
    /// and reproducing it is the whole point: the bug was invisible on a clean
    /// install.
    fn restaurant_only(into: &Path) -> PathBuf {
        let dataset = into.join("demo-before-the-brands");
        let assets = dataset.join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        let mut menu: serde_json::Value =
            serde_json::from_slice(&std::fs::read(demo_dir().join("menu.json")).unwrap()).unwrap();
        menu["availability"] = serde_json::json!([]);
        std::fs::write(
            dataset.join("menu.json"),
            serde_json::to_vec_pretty(&menu).unwrap(),
        )
        .unwrap();
        for entry in std::fs::read_dir(demo_dir().join("assets")).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                std::fs::copy(&path, assets.join(path.file_name().unwrap())).unwrap();
            }
        }
        dataset
    }

    /// A daemon on a throwaway database, stopped when the test ends.
    struct Core {
        process: CoreProcess,
        database: PathBuf,
        _dir: tempfile::TempDir,
    }

    impl Core {
        fn start() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let database = dir.path().join("pi-edge.db");
            let process = CoreProcess::default();
            ensure_core(&database, &process).expect("a daemon for the test");
            Self {
                process,
                database,
                _dir: dir,
            }
        }
    }

    impl Drop for Core {
        fn drop(&mut self) {
            self.process.shutdown();
        }
    }

    /// The regression: an installation on the old dataset has an active
    /// catalog and two blank lanes, and bootstrap has to fill them.
    #[test]
    fn an_installation_from_before_the_brands_gets_their_menus_on_the_next_launch() {
        let core = Core::start();
        let old = restaurant_only(core._dir.path());

        // What the previous release left behind.
        assert!(
            ensure_demo(&core.database, &old, None).unwrap(),
            "the old dataset did not install"
        );
        let blank = lanes_without_a_menu(&core.database);
        assert_eq!(
            blank,
            ["KIOSK-TIM", "KIOSK-SBARRO", "KIOSK-RETAIL"],
            "the old dataset was expected to leave exactly these lanes blank"
        );

        // The upgraded app launches. This is the call that used to answer
        // "already seeded" and leave Tim Hortons and Sbarro empty.
        assert!(
            ensure_demo(&core.database, &demo_dir(), None).unwrap(),
            "the upgrade installed nothing; the two brands stay blank"
        );
        assert!(
            lanes_without_a_menu(&core.database).is_empty(),
            "a lane this build ships still has no menu"
        );

        // And the shelves the acceptance criteria name, with a photograph on
        // every item: an upgraded installation has to end up where a clean
        // one does, not merely non-empty.
        use edge_client::EdgeClient;
        let client =
            edge_client::IpcEdgeClient::for_database(&core.database, "px-kiosk").expect("a client");
        for (channel, expected) in [("KIOSK-TIM", 13), ("KIOSK-SBARRO", 14)] {
            let menu = client
                .menu(Some(channel.to_owned()))
                .unwrap_or_else(|error| panic!("{channel}: {error:?}"));
            assert_eq!(menu.items.len(), expected, "{channel} shelf");
            assert_eq!(menu.categories.len(), 4, "{channel} aisles");
            for item in &menu.items {
                assert!(
                    item.image.is_some(),
                    "{} reached {channel} with no photograph",
                    item.product_ref
                );
                assert!(item.from_price.minor > 0, "{} is free", item.product_ref);
            }
        }
        // The two lanes that already worked still work.
        for channel in ["KIOSK", "KIOSK-RETAIL"] {
            assert!(
                !client
                    .menu(Some(channel.to_owned()))
                    .expect("a menu")
                    .items
                    .is_empty(),
                "the upgrade emptied {channel}"
            );
        }
    }

    /// And the launch after that costs nothing: the guard still has to stop a
    /// kiosk re-importing its catalog every morning.
    #[test]
    fn a_launch_with_every_lane_already_selling_installs_nothing() {
        let core = Core::start();
        assert!(ensure_demo(&core.database, &demo_dir(), None).unwrap());
        assert!(
            !ensure_demo(&core.database, &demo_dir(), None).unwrap(),
            "the second launch re-imported the catalog"
        );
        assert!(lanes_without_a_menu(&core.database).is_empty());
    }

    /// An edited price becomes the price the catalog in force charges.
    ///
    /// The whole claim of ADR 050, end to end against a real daemon: write
    /// the override, run the same pipeline a release runs, and ask the core
    /// what the product costs now. A number that only lived in a file would
    /// fail here.
    #[test]
    fn an_overridden_price_is_the_one_the_catalog_in_force_charges() {
        let core = Core::start();
        let overrides = core._dir.path().join("overrides.json");
        assert!(ensure_demo(&core.database, &demo_dir(), Some(&overrides)).unwrap());

        use edge_client::EdgeClient;
        let client =
            edge_client::IpcEdgeClient::for_database(&core.database, "px-kiosk").expect("a client");
        let price_of = |product_ref: &str| -> i64 {
            client
                .menu(Some("KIOSK-TIM".to_owned()))
                .expect("a menu")
                .items
                .iter()
                .find(|item| item.product_ref == product_ref)
                .unwrap_or_else(|| panic!("{product_ref} is not on the menu"))
                .from_price
                .minor
        };
        let shipped = price_of("T-LATTE");
        assert!(shipped > 0);

        // An operator changes it on the kiosk.
        std::fs::write(&overrides, r#"{"T-LATTE":{"price_minor":9900}}"#).unwrap();
        reseed(&core.database, &demo_dir(), Some(&overrides)).expect("the reimport");
        assert_eq!(
            price_of("T-LATTE"),
            9_900,
            "the override never reached the catalog the till reads"
        );

        // And it survives the kind of re-seed an upgrade performs, because
        // the overrides travel with it.
        reseed(&core.database, &demo_dir(), Some(&overrides)).expect("a second reimport");
        assert_eq!(price_of("T-LATTE"), 9_900);

        // Taking the override away puts the dataset's own price back.
        std::fs::write(&overrides, "{}").unwrap();
        reseed(&core.database, &demo_dir(), Some(&overrides)).expect("the third reimport");
        assert_eq!(price_of("T-LATTE"), shipped);
    }

    /// A database nothing has ever imported into is every lane blank, not an
    /// error — that is the clean install, and it must seed.
    #[test]
    fn a_fresh_installation_reports_every_lane_blank() {
        let core = Core::start();
        assert_eq!(
            lanes_without_a_menu(&core.database),
            ["KIOSK", "KIOSK-TIM", "KIOSK-SBARRO", "KIOSK-RETAIL"]
        );
    }
}
