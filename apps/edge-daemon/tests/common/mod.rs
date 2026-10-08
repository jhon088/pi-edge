//! Harness for driving a real `edge-daemon` process from a test.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use edge_catalog::{hash_chunk, hash_snapshot, CatalogBarcode, CatalogProduct, Currency};
use edge_client::{ClientError, EdgeClient, IpcEdgeClient};
use edge_protocol::{
    AddItemRequestV1, BeginCatalogImportRequestV1, CatalogProductRecordV1, CatalogSnapshotViewV1,
    CommitCatalogImportRequestV1, InitDeviceRequestV1, OpenRegisterSessionRequestV1,
    ProductSelectorV1, PutCatalogChunkRequestV1, SnapshotCountsV1, TaxProfileV1,
};

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

/// Barcode of the default demo product.
pub const DEMO_BARCODE: &str = "7501000000001";

/// A catalog snapshot, in the shape the server will eventually send.
pub struct CatalogSpec {
    pub version: i64,
    pub tax_profiles: Vec<TaxProfileV1>,
    pub products: Vec<CatalogProductRecordV1>,
}

impl CatalogSpec {
    /// One product: 100.00 MXN at 16%, tax exclusive.
    pub fn demo(version: i64) -> Self {
        Self::with_products(
            version,
            vec![product(DEMO_BARCODE, "SKU-001", 10_000, "IVA16")],
        )
    }

    pub fn with_products(version: i64, products: Vec<CatalogProductRecordV1>) -> Self {
        Self {
            version,
            tax_profiles: vec![
                tax_profile("IVA16", 1_600, "TAX_EXCLUSIVE"),
                tax_profile("IVA8", 800, "TAX_EXCLUSIVE"),
                tax_profile("IVA0", 0, "TAX_EXCLUSIVE"),
                tax_profile("IVA16-INC", 1_600, "TAX_INCLUSIVE"),
            ],
            products,
        }
    }

    fn domain(&self) -> (Vec<CatalogProduct>, Vec<CatalogBarcode>) {
        let mut products = Vec::new();
        let mut barcodes = Vec::new();
        for record in &self.products {
            let (product, mut codes) = record.clone().into_domain(Currency::Mxn).unwrap();
            products.push(product);
            barcodes.append(&mut codes);
        }
        (products, barcodes)
    }

    pub fn declared_hash(&self) -> String {
        let profiles: Vec<_> = self
            .tax_profiles
            .iter()
            .cloned()
            .map(TaxProfileV1::into_domain)
            .collect::<Result<_, _>>()
            .unwrap();
        let (products, barcodes) = self.domain();
        hash_snapshot(&profiles, &products, &barcodes)
    }

    pub fn begin_request(&self) -> BeginCatalogImportRequestV1 {
        let (products, barcodes) = self.domain();
        BeginCatalogImportRequestV1 {
            version: self.version,
            currency: "MXN".to_owned(),
            content_hash: self.declared_hash(),
            generated_at: "2026-10-01T00:00:00Z".to_owned(),
            effective_at: None,
            expected: SnapshotCountsV1 {
                tax_profiles: self.tax_profiles.len() as i64,
                products: products.len() as i64,
                barcodes: barcodes.len() as i64,
            },
            tax_profiles: self.tax_profiles.clone(),
        }
    }

    /// Chunk request for a slice of the products, with its canonical hash.
    pub fn chunk(
        &self,
        snapshot_id: &str,
        chunk_index: i64,
        records: &[CatalogProductRecordV1],
    ) -> PutCatalogChunkRequestV1 {
        let mut products = Vec::new();
        let mut barcodes = Vec::new();
        for record in records {
            let (product, mut codes) = record.clone().into_domain(Currency::Mxn).unwrap();
            products.push(product);
            barcodes.append(&mut codes);
        }
        PutCatalogChunkRequestV1 {
            snapshot_id: snapshot_id.to_owned(),
            chunk_index,
            chunk_hash: hash_chunk(&products, &barcodes),
            products: records.to_vec(),
        }
    }

    /// Import in one chunk, optionally activating it.
    pub fn import(&self, client: &IpcEdgeClient, activate: bool) -> CatalogSnapshotViewV1 {
        let opened = client
            .begin_catalog_import(self.begin_request(), None)
            .unwrap();
        let snapshot_id = opened.snapshot.snapshot_id.clone();
        client
            .put_catalog_chunk(self.chunk(&snapshot_id, 0, &self.products))
            .unwrap();
        let view = client
            .commit_catalog_import(
                CommitCatalogImportRequestV1 {
                    snapshot_id: snapshot_id.clone(),
                    chunk_count: 1,
                },
                None,
            )
            .unwrap();
        if activate {
            client
                .activate_catalog_snapshot(&snapshot_id, None)
                .unwrap()
        } else {
            view
        }
    }

    /// Write the snapshot as a file the CLI importer can read.
    pub fn write_fixture(&self, path: &Path) {
        let fixture = serde_json::json!({
            "version": self.version,
            "currency": "MXN",
            "generated_at": "2026-10-01T00:00:00Z",
            "tax_profiles": self.tax_profiles,
            "products": self.products,
        });
        std::fs::write(path, serde_json::to_vec_pretty(&fixture).unwrap()).unwrap();
    }
}

pub fn tax_profile(code: &str, rate_bps: u32, price_mode: &str) -> TaxProfileV1 {
    TaxProfileV1 {
        code: code.to_owned(),
        name: format!("profile {code}"),
        rate_bps,
        price_mode: price_mode.to_owned(),
    }
}

pub fn product(
    barcode: &str,
    product_ref: &str,
    price_minor: i64,
    tax_profile: &str,
) -> CatalogProductRecordV1 {
    CatalogProductRecordV1 {
        product_ref: product_ref.to_owned(),
        external_ref: None,
        sku: Some(product_ref.to_owned()),
        name: format!("Producto {product_ref}"),
        active: true,
        sellable: true,
        tax_profile: tax_profile.to_owned(),
        unit_price_minor: price_minor,
        barcodes: vec![barcode.to_owned()],
    }
}

/// Provision the device, import a catalog, open the day and open a session.
pub fn open_register(client: &IpcEdgeClient) {
    client
        .init_device(InitDeviceRequestV1 {
            tenant_id: "TENANT-DEMO".to_owned(),
            store_id: "STORE-DEMO".to_owned(),
            currency: "MXN".to_owned(),
        })
        .unwrap();
    CatalogSpec::demo(1).import(client, true);
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

/// Scan the demo product: the core resolves 100.00 MXN at 16%.
pub fn demo_item() -> AddItemRequestV1 {
    AddItemRequestV1 {
        product: ProductSelectorV1::Barcode(DEMO_BARCODE.to_owned()),
        quantity_thousandths: 1_000,
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
