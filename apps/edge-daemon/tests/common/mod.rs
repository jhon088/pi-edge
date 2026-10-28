//! Harness for driving a real `edge-daemon` process from a test.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use edge_catalog::{
    hash_chunk, hash_snapshot, CatalogBarcode, CatalogProduct, Category, Channel,
    ChannelAvailability, ChunkContent, Currency, PriceEntry, ProductPresentation, Promotion,
    SelectionGroup, SelectionOption, SnapshotContent,
};
use edge_client::{ClientError, EdgeClient, IpcEdgeClient};
use edge_protocol::{
    AddItemRequestV1, BeginCatalogImportRequestV1, CatalogProductRecordV1, CatalogSnapshotViewV1,
    CategoryV1, ChannelAvailabilityV1, ChannelV1, CommitCatalogImportRequestV1,
    ImportInventoryBootstrapRequestV1, InitDeviceRequestV1, InventoryBootstrapItemV1,
    InventoryBootstrapV1, InventoryPositionV1, InventoryQueryV1, OpenRegisterSessionRequestV1,
    PriceBookV1, PriceRecordV1, ProductPresentationV1, ProductSelectorV1, PromotionRecordV1,
    PromotionRuleV1, PutCatalogChunkRequestV1, SelectionGroupV1, SelectionOptionV1,
    SnapshotCountsV1, StartPaymentRequestV1, TaxProfileV1,
};

/// The extensions of one spec, converted once into the domain so the hash is
/// computed from exactly what the daemon will persist.
struct DomainExtensions {
    channels: Vec<Channel>,
    categories: Vec<Category>,
    presentation: Vec<ProductPresentation>,
    selection_groups: Vec<SelectionGroup>,
    selection_options: Vec<SelectionOption>,
    availability: Vec<ChannelAvailability>,
}

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
        Self::start_with_payment_script(database, None)
    }

    /// Start a daemon whose card provider follows a script, so a declined or
    /// lost provider call is reproducible over the real transport.
    pub fn start_with_payment_script(database: &Path, script: Option<&str>) -> Self {
        Self::start_configured(database, script, true)
    }

    /// Start a daemon, optionally **without** refund authority, which is the
    /// production default: no authority source, no refunds (ADR 030).
    pub fn start_configured(database: &Path, script: Option<&str>, refund_authority: bool) -> Self {
        Self::start_with_env(database, script, refund_authority, &[])
    }

    /// Start a daemon that is **expected to refuse**, answering with what it
    /// said on stderr.
    ///
    /// A misconfigured runtime must not come up quietly: it names the setting
    /// it could not understand and exits.
    pub fn start_failing(database: &Path, env: &[(&str, &str)]) -> String {
        let mut command = Command::new(env!("CARGO_BIN_EXE_edge-daemon"));
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command
            .arg("--db")
            .arg(database)
            .env("PI_EDGE_LOG", "error")
            .output()
            .expect("edge-daemon binary should be built before this test runs");
        assert!(
            !output.status.success(),
            "the daemon started when it should have refused"
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    /// Start a daemon with extra configuration, for the hardware drivers.
    pub fn start_with_env(
        database: &Path,
        script: Option<&str>,
        refund_authority: bool,
        env: &[(&str, &str)],
    ) -> Self {
        let log_path = database.with_extension("daemon.log");
        let log = std::fs::File::create(&log_path).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_edge-daemon"));
        if let Some(script) = script {
            command.env("PI_EDGE_PAYMENT_SIM", script);
        }
        if refund_authority {
            command.env("PI_EDGE_REFUND_AUTHORITY", "dev-accept-local");
        }
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command
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

    /// Wait for the process to be gone, for cases that expect it to die.
    ///
    /// A dropped connection and a reaped child are two different events, and
    /// the first can be observed before the second.
    pub fn wait_until_dead(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !self.is_alive() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
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
///
/// `CARGO_BIN_EXE_<name>` is the right answer for a binary in the *same*
/// package as the test, which is how `edge-daemon` is found below. It is not
/// available for `edge-cli`, which lives in another package, and Cargo's
/// declarative answer to that — artifact dependencies, `artifact = "bin"` —
/// is still nightly-only. So on stable this test has to satisfy its own build
/// dependency rather than assume somebody else did.
///
/// It cannot assume it: `cargo test --workspace` builds a package's binaries
/// only when that package has test targets to build them for, and `edge-cli`
/// has none. A machine where it happens to exist is one where some earlier
/// `cargo build` left it behind — which is every development machine, and no
/// clean CI runner.
pub fn cli_binary() -> PathBuf {
    // Once per test binary. Every test in it that shells out to the CLI wants
    // the same path, and `cargo build` takes a package lock — so asking per
    // test would queue every thread behind the first one for nothing.
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT.get_or_init(build_cli).clone()
}

fn build_cli() -> PathBuf {
    // Same directory, same profile, whatever profile that is.
    let daemon = PathBuf::from(env!("CARGO_BIN_EXE_edge-daemon"));
    let dir = daemon.parent().expect("a built binary has a directory");
    let candidate = dir.join(format!("edge-cli{}", std::env::consts::EXE_SUFFIX));

    // Built every time, not only when missing. Cargo never builds `edge-cli`
    // as a side effect of testing `edge-daemon`, so a binary left over from an
    // earlier build is *stale*, and a stale CLI makes these tests assert the
    // behaviour of code that is no longer in the tree — green for a bug, red
    // for a fix. A no-op `cargo build` costs a fraction of a second; believing
    // an old binary costs an afternoon.
    //
    // `CARGO` is set for exactly this, so nothing here guesses at an
    // executable or a layout.
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let mut build = Command::new(cargo);
    build
        .arg("build")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--package")
        .arg("edge-cli");
    // Match the profile the test itself was built with, read off the path
    // rather than assumed, so `cargo test --release` puts it in the same
    // place this function is about to look.
    if dir.file_name().is_some_and(|name| name == "release") {
        build.arg("--release");
    }
    let status = build
        .status()
        .unwrap_or_else(|error| panic!("could not run cargo to build edge-cli: {error}"));
    assert!(status.success(), "building edge-cli failed");
    assert!(
        candidate.exists(),
        "edge-cli was built but is not at {}",
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

    /// Wait for the process to be gone, for cases that expect it to die.
    ///
    /// A dropped connection and a reaped child are two different events, and
    /// the first can be observed before the second.
    pub fn wait_until_dead(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !self.is_alive() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
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
    pub price_books: Vec<PriceBookV1>,
    pub products: Vec<CatalogProductRecordV1>,
    pub promotions: Vec<PromotionRecordV1>,
    /// Universal catalog extensions (ADR 044). Empty for every Retail spec,
    /// which is what every earlier milestone's test builds.
    pub channels: Vec<ChannelV1>,
    pub categories: Vec<CategoryV1>,
    pub presentation: Vec<ProductPresentationV1>,
    pub selection_groups: Vec<SelectionGroupV1>,
    pub selection_options: Vec<SelectionOptionV1>,
    pub availability: Vec<ChannelAvailabilityV1>,
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
                tax_profile("IVA16", 1_600),
                tax_profile("IVA8", 800),
                tax_profile("IVA0", 0),
            ],
            price_books: vec![base_book()],
            products,
            promotions: Vec::new(),
            channels: Vec::new(),
            categories: Vec::new(),
            presentation: Vec::new(),
            selection_groups: Vec::new(),
            selection_options: Vec::new(),
            availability: Vec::new(),
        }
    }

    /// Add a promotion, the way the snapshot would carry it.
    pub fn promoting(mut self, promotion: PromotionRecordV1) -> Self {
        self.promotions.push(promotion);
        self
    }

    /// Declare a way of selling (ADR 044).
    pub fn with_channel(mut self, channel: ChannelV1) -> Self {
        self.channels.push(channel);
        self
    }

    pub fn with_category(mut self, category: CategoryV1) -> Self {
        self.categories.push(category);
        self
    }

    pub fn with_presentation(mut self, presentation: ProductPresentationV1) -> Self {
        self.presentation.push(presentation);
        self
    }

    /// One question and its answers: a group never travels without them.
    pub fn with_selection(
        mut self,
        group: SelectionGroupV1,
        options: Vec<SelectionOptionV1>,
    ) -> Self {
        self.selection_groups.push(group);
        self.selection_options.extend(options);
        self
    }

    pub fn with_availability(mut self, row: ChannelAvailabilityV1) -> Self {
        self.availability.push(row);
        self
    }

    fn domain_extensions(&self) -> DomainExtensions {
        DomainExtensions {
            channels: self
                .channels
                .iter()
                .cloned()
                .map(|value| value.into_domain().unwrap())
                .collect(),
            categories: self
                .categories
                .iter()
                .cloned()
                .map(|value| value.into_domain().unwrap())
                .collect(),
            presentation: self
                .presentation
                .iter()
                .cloned()
                .map(|value| value.into_domain().unwrap())
                .collect(),
            selection_groups: self
                .selection_groups
                .iter()
                .cloned()
                .map(|value| value.into_domain().unwrap())
                .collect(),
            selection_options: self
                .selection_options
                .iter()
                .cloned()
                .map(|value| value.into_domain().unwrap())
                .collect(),
            availability: self
                .availability
                .iter()
                .cloned()
                .map(|value| value.into_domain().unwrap())
                .collect(),
        }
    }

    fn domain_promotions(&self) -> Vec<Promotion> {
        self.promotions
            .iter()
            .cloned()
            .map(|record| record.into_domain(Currency::Mxn).unwrap())
            .collect()
    }

    fn domain(&self) -> (Vec<CatalogProduct>, Vec<PriceEntry>, Vec<CatalogBarcode>) {
        let mut products = Vec::new();
        let mut prices = Vec::new();
        let mut barcodes = Vec::new();
        for record in &self.products {
            let mut parsed = record.clone().into_domain(Currency::Mxn).unwrap();
            products.push(parsed.product);
            prices.append(&mut parsed.prices);
            barcodes.append(&mut parsed.barcodes);
        }
        (products, prices, barcodes)
    }

    pub fn declared_hash(&self) -> String {
        let profiles: Vec<_> = self
            .tax_profiles
            .iter()
            .cloned()
            .map(TaxProfileV1::into_domain)
            .collect::<Result<_, _>>()
            .unwrap();
        let books: Vec<_> = self
            .price_books
            .iter()
            .cloned()
            .map(PriceBookV1::into_domain)
            .collect::<Result<_, _>>()
            .unwrap();
        let (products, price_entries, barcodes) = self.domain();
        let extensions = self.domain_extensions();
        hash_snapshot(SnapshotContent {
            tax_profiles: &profiles,
            price_books: &books,
            products: &products,
            price_entries: &price_entries,
            barcodes: &barcodes,
            promotions: &self.domain_promotions(),
            channels: &extensions.channels,
            categories: &extensions.categories,
            presentation: &extensions.presentation,
            selection_groups: &extensions.selection_groups,
            selection_options: &extensions.selection_options,
            availability: &extensions.availability,
        })
    }

    pub fn begin_request(&self) -> BeginCatalogImportRequestV1 {
        let (products, price_entries, barcodes) = self.domain();
        BeginCatalogImportRequestV1 {
            version: self.version,
            currency: "MXN".to_owned(),
            content_hash: self.declared_hash(),
            generated_at: "2026-10-01T00:00:00Z".to_owned(),
            effective_at: None,
            expected: SnapshotCountsV1 {
                tax_profiles: self.tax_profiles.len() as i64,
                price_books: self.price_books.len() as i64,
                products: products.len() as i64,
                price_entries: price_entries.len() as i64,
                barcodes: barcodes.len() as i64,
                promotions: self.promotions.len() as i64,
                channels: self.channels.len() as i64,
                categories: self.categories.len() as i64,
                presentation: self.presentation.len() as i64,
                selection_groups: self.selection_groups.len() as i64,
                selection_options: self.selection_options.len() as i64,
                availability: self.availability.len() as i64,
            },
            tax_profiles: self.tax_profiles.clone(),
            price_books: self.price_books.clone(),
            channels: self.channels.clone(),
            categories: self.categories.clone(),
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
        let mut prices = Vec::new();
        let mut barcodes = Vec::new();
        for record in records {
            let mut parsed = record.clone().into_domain(Currency::Mxn).unwrap();
            products.push(parsed.product);
            prices.append(&mut parsed.prices);
            barcodes.append(&mut parsed.barcodes);
        }
        // Single-chunk imports in these tests, so every promotion rides here.
        let promotions = self.domain_promotions();
        let extensions = self.domain_extensions();
        PutCatalogChunkRequestV1 {
            snapshot_id: snapshot_id.to_owned(),
            chunk_index,
            chunk_hash: hash_chunk(ChunkContent {
                products: &products,
                price_entries: &prices,
                barcodes: &barcodes,
                promotions: &promotions,
                presentation: &extensions.presentation,
                selection_groups: &extensions.selection_groups,
                selection_options: &extensions.selection_options,
                availability: &extensions.availability,
            }),
            products: records.to_vec(),
            promotions: self.promotions.clone(),
            presentation: self.presentation.clone(),
            selection_groups: self.selection_groups.clone(),
            selection_options: self.selection_options.clone(),
            availability: self.availability.clone(),
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
            "price_books": self.price_books,
            "products": self.products,
        });
        std::fs::write(path, serde_json::to_vec_pretty(&fixture).unwrap()).unwrap();
    }
}

pub fn tax_profile(code: &str, rate_bps: u32) -> TaxProfileV1 {
    TaxProfileV1 {
        code: code.to_owned(),
        name: format!("profile {code}"),
        rate_bps,
    }
}

/// The tenant-wide base book every spec starts with.
pub fn base_book() -> PriceBookV1 {
    PriceBookV1 {
        price_book_id: "BASE".to_owned(),
        code: "BASE".to_owned(),
        name: "Base".to_owned(),
        currency: "MXN".to_owned(),
        scope: "TENANT".to_owned(),
        store_id: None,
        priority: 0,
        active: true,
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
        barcodes: vec![barcode.to_owned()],
        prices: vec![PriceRecordV1 {
            price_book: "BASE".to_owned(),
            amount_minor: price_minor,
            price_mode: "TAX_EXCLUSIVE".to_owned(),
            valid_from: None,
            valid_until: None,
        }],
    }
}

/// Provision the device, import a catalog, open the day and open a session.
pub fn open_register(client: &IpcEdgeClient) {
    open_register_with_policy(client, true);
}

/// Same, stating the negative-inventory policy of the installation.
pub fn open_register_with_policy(client: &IpcEdgeClient, allow_negative_inventory: bool) {
    client
        .init_device(InitDeviceRequestV1 {
            tenant_id: "TENANT-DEMO".to_owned(),
            store_id: "STORE-DEMO".to_owned(),
            currency: "MXN".to_owned(),
            allow_negative_inventory,
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

/// Import an opening position over the wire: `(product_ref, units)` pairs.
pub fn bootstrap_stock(client: &IpcEdgeClient, items: &[(&str, i64)]) -> InventoryBootstrapV1 {
    client
        .import_inventory_bootstrap(
            ImportInventoryBootstrapRequestV1 {
                source_snapshot_id: "WMS-SNAPSHOT-1".to_owned(),
                generated_at: "2026-10-08T14:00:00Z".to_owned(),
                items: items
                    .iter()
                    .map(|(product_ref, units)| InventoryBootstrapItemV1 {
                        product_ref: (*product_ref).to_owned(),
                        quantity_thousandths: units * 1_000,
                    })
                    .collect(),
            },
            None,
        )
        .unwrap()
}

/// One product's position, with its movements.
pub fn inventory_position(client: &IpcEdgeClient, product_ref: &str) -> InventoryPositionV1 {
    client
        .inventory_position(InventoryQueryV1 {
            product_ref: product_ref.to_owned(),
            movement_limit: 50,
            offset: 0,
        })
        .unwrap()
}

/// A finished cash sale over the wire, answering with its id.
pub fn finalized_cash_sale(client: &IpcEdgeClient, tendered_minor: i64) -> String {
    client.start_transaction(None).unwrap();
    client.add_item(demo_item(), None).unwrap();
    client.totalize(None).unwrap();
    client
        .start_payment(
            StartPaymentRequestV1 {
                tender_type: "CASH".to_owned(),
                amount_minor: tendered_minor,
            },
            None,
        )
        .unwrap();
    client.finalize_transaction(None).unwrap().transaction_id
}

/// Scan the demo product: the core resolves 100.00 MXN at 16%.
pub fn demo_item() -> AddItemRequestV1 {
    AddItemRequestV1 {
        product: ProductSelectorV1::Barcode(DEMO_BARCODE.to_owned()),
        quantity_thousandths: 1_000,
        selections: Vec::new(),
        preparation_note: None,
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

/// A promotion record in the shape the server will send.
pub fn promotion_record(
    promotion_id: &str,
    priority: i32,
    stackability: &str,
    rule: PromotionRuleV1,
) -> PromotionRecordV1 {
    PromotionRecordV1 {
        promotion_id: promotion_id.to_owned(),
        code: promotion_id.to_owned(),
        name: format!("Promoción {promotion_id}"),
        active: true,
        priority,
        stackability: stackability.to_owned(),
        scope: "TENANT".to_owned(),
        store_id: None,
        valid_from: None,
        valid_until: None,
        rule,
    }
}

pub fn percentage_off_record(promotion_id: &str, product_ref: &str, bps: u32) -> PromotionRecordV1 {
    promotion_record(
        promotion_id,
        0,
        "STACKABLE",
        PromotionRuleV1::PercentageOff {
            product_ref: Some(product_ref.to_owned()),
            percentage_bps: bps,
        },
    )
}
