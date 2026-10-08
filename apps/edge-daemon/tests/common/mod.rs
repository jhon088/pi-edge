//! Harness for driving a real `edge-daemon` process from a test.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use edge_client::{ClientError, EdgeClient, IpcEdgeClient};
use edge_protocol::{AddItemRequestV1, InitDeviceRequestV1, OpenRegisterSessionRequestV1};

/// A spawned daemon process. Killed on drop so a failing test cannot leak a
/// runtime that would block the next one.
pub struct DaemonProcess {
    child: Child,
    stdout: Option<BufReader<ChildStdout>>,
    pub endpoint: String,
    pub pid: u32,
    pub database: PathBuf,
    pub log_path: PathBuf,
}

impl DaemonProcess {
    /// Start a daemon and wait for its readiness line.
    pub fn start(database: &Path) -> Self {
        let log_path = database.with_extension("daemon.log");
        let log = std::fs::File::create(&log_path).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_edge-daemon"))
            .arg("--db")
            .arg(database)
            .env("PI_EDGE_LOG", "debug")
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .expect("edge-daemon binary should be built before this test runs");

        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        // The pipe closes if the daemon dies, so this cannot hang forever.
        stdout
            .read_line(&mut line)
            .expect("daemon should print a readiness line");
        let ready: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_else(|error| {
            panic!("readiness line was not JSON ({error}): {line:?}");
        });
        assert_eq!(
            ready["status"], "READY",
            "unexpected readiness line: {ready}"
        );

        Self {
            pid: child.id(),
            child,
            stdout: Some(stdout),
            endpoint: ready["endpoint"].as_str().unwrap().to_owned(),
            database: database.to_path_buf(),
            log_path,
        }
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Simulate a power cut or a killed process.
    pub fn kill_hard(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Ask for a graceful shutdown and wait for the exit code.
    #[cfg(unix)]
    pub fn stop_gracefully(&mut self) -> i32 {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.pid.to_string())
            .status();
        self.child.wait().unwrap().code().unwrap_or(-1)
    }

    pub fn stderr_contents(&self) -> String {
        std::fs::read_to_string(&self.log_path).unwrap_or_default()
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Path of the sibling `edge-cli` binary, used as a real second process.
pub fn cli_binary() -> PathBuf {
    let daemon = PathBuf::from(env!("CARGO_BIN_EXE_edge-daemon"));
    let candidate = daemon
        .parent()
        .unwrap()
        .join(format!("edge-cli{}", std::env::consts::EXE_SUFFIX));
    assert!(
        candidate.exists(),
        "edge-cli must be built first (cargo test --workspace does): {}",
        candidate.display()
    );
    candidate
}

/// A separate process subscribed to the core's event stream — the role the
/// Tauri UI plays. Killing it is a genuine UI death, not a dropped object.
pub struct WatchProcess {
    child: Child,
    stdout: BufReader<ChildStdout>,
}

impl WatchProcess {
    pub fn start(database: &Path, count: usize) -> Self {
        let mut child = Command::new(cli_binary())
            .arg("--db")
            .arg(database)
            .arg("--format")
            .arg("json")
            .arg("watch")
            .arg("--count")
            .arg(count.to_string())
            .env("PI_EDGE_LOG", "warn")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_else(|error| {
            panic!("watch readiness line was not JSON ({error}): {line:?}")
        });
        assert_eq!(ready["watching"], true, "unexpected line: {ready}");
        Self { child, stdout }
    }

    /// Next pushed event, as the UI process saw it.
    pub fn next_event(&mut self) -> serde_json::Value {
        let mut line = String::new();
        let read = self.stdout.read_line(&mut line).unwrap();
        assert!(
            read > 0,
            "the event stream closed without delivering an event"
        );
        serde_json::from_str(line.trim()).unwrap()
    }

    pub fn kill_hard(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for WatchProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn client(database: &Path, name: &'static str) -> IpcEdgeClient {
    IpcEdgeClient::for_database(database, name).unwrap()
}

/// Provision the device, open the day and open a session on `REG-001`.
pub fn open_register(client: &IpcEdgeClient) {
    client
        .init_device(InitDeviceRequestV1 {
            tenant_id: "TENANT-DEMO".to_owned(),
            store_id: "STORE-DEMO".to_owned(),
            currency: "MXN".to_owned(),
        })
        .unwrap();
    client.open_business_day(None).unwrap();
    client
        .open_session(
            OpenRegisterSessionRequestV1 {
                register_id: "REG-001".to_owned(),
                operator_id: "CASHIER-001".to_owned(),
                opening_cash_minor: 100_000,
            },
            None,
        )
        .unwrap();
}

/// 100.00 MXN at 16% tax.
pub fn demo_item() -> AddItemRequestV1 {
    AddItemRequestV1 {
        product_ref: "SKU-001".to_owned(),
        sku: Some("SKU-001".to_owned()),
        barcode: None,
        description: "Producto prueba".to_owned(),
        quantity_thousandths: 1_000,
        unit_price_minor: 10_000,
        tax_rate_bps: 1_600,
        discount_minor: 0,
    }
}

/// Retry a query until the client reconnects, honouring its backoff.
pub fn wait_until_connected<T, F>(mut call: F, timeout: Duration) -> T
where
    F: FnMut() -> Result<T, ClientError>,
{
    let deadline = Instant::now() + timeout;
    let mut last = None;
    while Instant::now() < deadline {
        match call() {
            Ok(value) => return value,
            Err(error) => {
                last = Some(error);
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    panic!("client never reconnected: {last:?}");
}
