//! Shared fixtures for the PI.Edge integration tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::{DateTime, Utc};
use edge_application::commands::*;
use edge_application::ports::InventoryBootstrapState;
use edge_application::ports::{EdgeStore, OutboxCounts};
use edge_application::queries::{InventoryPositionView, RefundView};
use edge_application::sync_service::SyncRuntime;
use edge_application::testing::{
    CatalogFixture, FixedClock, FixtureProduct, SequentialIdFactory, DEMO_BARCODE,
};
use edge_application::EdgeApplication;
use edge_assets::FileSystemAssetStore;
use edge_catalog::promotions::{
    NewPromotion, ProductApplicability, Promotion, PromotionRule, Stackability,
};
use edge_catalog::scope::CatalogScope;
use edge_catalog::{AssetRef, Barcode, ProductSelector};
use edge_domain::prelude::*;
use edge_inventory::BootstrapItem;
use edge_payments::{SimulatorPaymentProvider, SimulatorScript, TenderType};
use edge_refunds::{
    RefundAuthority, RefundAuthorizationContext, RefundFiscalStatus, RefundLineRequest,
};
use edge_storage::SqliteEdgeStore;
use edge_sync::simulator::SimulatorSyncTransport;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::Arc;

use edge_application::hardware_service::Hardware;
use edge_hardware::{ScanEvent, SimulatorDrawer, SimulatorPrinter};

pub use edge_payments::TenderType as Tender;

pub type TestCore = EdgeApplication<SqliteEdgeStore, FixedClock, SequentialIdFactory>;

/// Fixed start of simulated time for every test.
pub const START: &str = "2026-10-08T15:00:00Z";

pub fn start_instant() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(START)
        .unwrap()
        .with_timezone(&Utc)
}

pub fn core_with_store(store: SqliteEdgeStore) -> TestCore {
    // Simulators are asked for **explicitly**, here and everywhere: a test
    // that uses fake hardware says so, and a core with nothing configured
    // answers `HARDWARE_NOT_CONFIGURED` (ADR 038).
    EdgeApplication::with_hardware(
        store,
        FixedClock::new(start_instant(), 1_000),
        SequentialIdFactory::new(),
        SimulatorPaymentProvider::approving(),
        Hardware::simulated(),
    )
}

pub fn in_memory_core() -> TestCore {
    core_with_store(SqliteEdgeStore::open_in_memory().unwrap())
}

/// A core with a real local asset store, in a directory that lives as long as
/// the returned guard.
///
/// Explicit, like every other adapter: a test that needs asset bytes says so,
/// and `in_memory_core` keeps the honest default of having no store at all
/// (ADR 045).
pub fn core_with_assets() -> (TestCore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSystemAssetStore::open(dir.path().join("assets")).unwrap();
    let core = in_memory_core().with_assets(Arc::new(store));
    (core, dir)
}

/// The bytes of a tiny, valid PNG, parameterised so a test can have more than
/// one distinct asset.
pub fn png_bytes(tag: u8) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend((0..64_u8).map(|index| index.wrapping_mul(tag).wrapping_add(tag)));
    bytes
}

/// The `AssetRef` those bytes answer to. Computed, never asserted: the hash
/// is the identity, so a test that hard-coded one would be testing itself.
pub fn asset_ref_for(bytes: &[u8]) -> AssetRef {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let mut hex = String::new();
    for byte in hasher.finalize() {
        hex.push_str(&format!("{byte:02x}"));
    }
    AssetRef::import(
        format!("sha256:{hex}"),
        "image/png",
        i64::try_from(bytes.len()).unwrap(),
    )
    .unwrap()
}

/// Put one asset in the store the way a transport would: chunks, then commit.
pub fn store_asset(core: &TestCore, bytes: &[u8], chunk: usize) -> AssetRef {
    let asset = asset_ref_for(bytes);
    let mut offset = 0_i64;
    for slice in bytes.chunks(chunk.max(1)) {
        core.put_asset_chunk(PutAssetChunkCommand {
            asset: asset.clone(),
            offset,
            bytes: slice.to_vec(),
        })
        .unwrap();
        offset += i64::try_from(slice.len()).unwrap();
    }
    let status = core
        .commit_asset(CommitAssetCommand {
            asset: asset.clone(),
        })
        .unwrap();
    assert!(status.state.is_ready(), "the asset is stored");
    asset
}

/// Read an asset back through the core, as a client eventually will.
pub fn read_asset(core: &TestCore, asset: &AssetRef) -> Vec<u8> {
    let mut bytes = Vec::new();
    core.open_asset(asset)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

pub fn init_device(core: &TestCore) {
    init_device_with_policy(core, true);
}

/// Provision the device, stating the negative-inventory policy explicitly.
///
/// The permissive default is what a shop floor wants; `false` is the strict
/// installation that refuses a sale the ledger cannot cover (ADR 029).
pub fn init_device_with_policy(core: &TestCore, allow_negative_inventory: bool) {
    core.init_device(InitDeviceCommand {
        tenant_id: TenantId::new("TENANT-DEMO").unwrap(),
        store_id: StoreId::new("STORE-DEMO").unwrap(),
        currency: Currency::Mxn,
        allow_negative_inventory,
    })
    .unwrap();
}

/// Import and activate the default demo catalog.
pub fn activate_demo_catalog(core: &TestCore) {
    CatalogFixture::demo(1).activate(core).unwrap();
}

/// Open the business day and a session on `REG-001`, assuming the device and
/// a catalog are already in place.
pub fn open_day_and_session(core: &TestCore) {
    core.open_business_day(OpenBusinessDayCommand::default())
        .unwrap();
    core.open_register_session(OpenRegisterSessionCommand {
        request_id: None,
        register_id: RegisterId::new("REG-001").unwrap(),
        operator_id: OperatorId::new("CASHIER-001").unwrap(),
        opening_cash_minor: 100_000,
    })
    .unwrap();
}

/// Device + catalog + open business day + open session on `REG-001`.
pub fn open_register(core: &TestCore) {
    init_device(core);
    activate_demo_catalog(core);
    core.open_business_day(OpenBusinessDayCommand::default())
        .unwrap();
    core.open_register_session(OpenRegisterSessionCommand {
        request_id: None,
        register_id: RegisterId::new("REG-001").unwrap(),
        operator_id: OperatorId::new("CASHIER-001").unwrap(),
        opening_cash_minor: 100_000,
    })
    .unwrap();
}

/// Scan the demo product: 100.00 MXN at 16%, resolved by the core.
pub fn demo_item() -> AddItemCommand {
    scan(DEMO_BARCODE, 1)
}

/// Add an item by barcode, the way a scanner would.
pub fn scan(barcode: &str, qty_units: i64) -> AddItemCommand {
    AddItemCommand {
        request_id: None,
        selector: ProductSelector::Barcode(Barcode::new(barcode).unwrap()),
        quantity: Quantity::from_units(qty_units).unwrap(),
        selections: Vec::new(),
        preparation_note: None,
    }
}

/// Add an item by canonical product reference.
pub fn item(product_ref: &str, qty_units: i64) -> AddItemCommand {
    AddItemCommand {
        request_id: None,
        selector: ProductSelector::ProductRef(ProductRef::new(product_ref).unwrap()),
        quantity: Quantity::from_units(qty_units).unwrap(),
        selections: Vec::new(),
        preparation_note: None,
    }
}

/// A catalog with the products the pricing tests need.
pub fn pricing_catalog(version: i64) -> CatalogFixture {
    CatalogFixture::demo(version)
        .with(
            FixtureProduct::new("SKU-002", "Segundo producto", 5_000)
                .sku("SKU-002")
                .barcode("7501000000002"),
        )
        .with(
            FixtureProduct::new("SKU-INACTIVE", "Descontinuado", 1_000)
                .barcode("7501000000003")
                .inactive(),
        )
        .with(
            FixtureProduct::new("SKU-SERVICE", "No vendible", 1_000)
                .barcode("7501000000004")
                .not_sellable(),
        )
}

/// Journal event types, oldest first.
pub fn journal_types(core: &TestCore) -> Vec<String> {
    let mut events = core.get_journal(100).unwrap();
    events.reverse();
    events
        .iter()
        .map(|e| e.event_type.as_str().to_owned())
        .collect()
}

/// Cash tender through the payment engine, answering with the sale.
///
/// Cash has no provider, so this is one database transaction and the payment
/// is committed when it returns.
pub fn pay_cash(core: &TestCore, amount_minor: i64) -> RetailTransaction {
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Cash,
        amount_minor,
    })
    .unwrap()
    .transaction
}

/// Core whose card provider follows a script, for the paths a real terminal
/// would produce: a refusal, and a call whose result is lost.
pub fn scripted_core(script: SimulatorScript) -> ScriptedCore {
    EdgeApplication::with_hardware(
        SqliteEdgeStore::open_in_memory().unwrap(),
        FixedClock::new(start_instant(), 1_000),
        SequentialIdFactory::new(),
        SimulatorPaymentProvider::new(script),
        Hardware::simulated(),
    )
}

pub type ScriptedCore =
    EdgeApplication<SqliteEdgeStore, FixedClock, SequentialIdFactory, SimulatorPaymentProvider>;

/// Open a register with an explicit float, so cash tests can state the figure
/// they reason about.
pub fn open_register_with_float(core: &TestCore, opening_cash_minor: i64) {
    init_device(core);
    activate_demo_catalog(core);
    core.open_business_day(OpenBusinessDayCommand::default())
        .unwrap();
    core.open_register_session(OpenRegisterSessionCommand {
        request_id: None,
        register_id: RegisterId::new("REG-001").unwrap(),
        operator_id: OperatorId::new("CASHIER-001").unwrap(),
        opening_cash_minor,
    })
    .unwrap();
}

/// A finished cash sale of the demo product: 116.00, tendered `tendered_minor`.
pub fn cash_sale(core: &TestCore, tendered_minor: i64) {
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Cash,
        amount_minor: tendered_minor,
    })
    .unwrap();
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
}

/// A register on an installation that refuses to sell into negative stock.
pub fn open_strict_register(core: &TestCore) {
    init_device_with_policy(core, false);
    activate_demo_catalog(core);
    open_day_and_session(core);
}

/// Import an opening position: `(product_ref, units)` pairs.
pub fn bootstrap_stock(core: &TestCore, items: &[(&str, i64)]) -> InventoryBootstrapState {
    core.import_inventory_bootstrap(bootstrap_command("WMS-SNAPSHOT-1", items))
        .unwrap()
}

/// The command a bootstrap import would arrive as, for the tests that need to
/// replay it or send a competing one.
pub fn bootstrap_command(
    source_snapshot_id: &str,
    items: &[(&str, i64)],
) -> ImportInventoryBootstrapCommand {
    ImportInventoryBootstrapCommand {
        request_id: None,
        source_snapshot_id: source_snapshot_id.to_owned(),
        generated_at: Timestamp::from_datetime(start_instant()),
        items: items
            .iter()
            .map(|(product_ref, units)| BootstrapItem {
                product_ref: ProductRef::new(*product_ref).unwrap(),
                quantity: Quantity::from_units(*units).unwrap(),
            })
            .collect(),
    }
}

/// On hand in thousandths, straight from the core's own fold.
pub fn on_hand(core: &TestCore, product_ref: &str) -> i64 {
    position(core, product_ref).position.on_hand_thousandths
}

pub fn position(core: &TestCore, product_ref: &str) -> InventoryPositionView {
    core.get_inventory_position(ProductRef::new(product_ref).unwrap(), 100)
        .unwrap()
}

/// The demo catalog plus a buy-2-get-1 on the demo product, so a refund of a
/// promotional line is testable end to end.
pub fn promotional_catalog(version: i64) -> CatalogFixture {
    CatalogFixture::demo(version).promoting(
        Promotion::import(NewPromotion {
            promotion_id: PromotionId::new("BOGO").unwrap(),
            code: "BOGO".to_owned(),
            name: "Lleva 3, paga 2".to_owned(),
            active: true,
            priority: 0,
            stackability: Stackability::Stackable,
            scope: CatalogScope::Tenant,
            valid_from: None,
            valid_until: None,
            rule: PromotionRule::BuyNGetM {
                applies_to: ProductApplicability::Product {
                    product_ref: ProductRef::new("SKU-001").unwrap(),
                },
                buy: Quantity::from_units(2).unwrap(),
                get: Quantity::from_units(1).unwrap(),
            },
        })
        .unwrap(),
    )
}

/// Provision a scripted core the way `open_register` provisions a plain one.
pub fn open_scripted_register(core: &ScriptedCore) {
    core.init_device(InitDeviceCommand {
        tenant_id: TenantId::new("TENANT-DEMO").unwrap(),
        store_id: StoreId::new("STORE-DEMO").unwrap(),
        currency: Currency::Mxn,
        allow_negative_inventory: true,
    })
    .unwrap();
    CatalogFixture::demo(1).activate(core).unwrap();
    core.open_business_day(OpenBusinessDayCommand::default())
        .unwrap();
    core.open_register_session(OpenRegisterSessionCommand {
        request_id: None,
        register_id: RegisterId::new("REG-001").unwrap(),
        operator_id: OperatorId::new("CASHIER-001").unwrap(),
        opening_cash_minor: 100_000,
    })
    .unwrap();
}

// --------------------------------------------------------------- refunds

/// The authority a refund needs, as the composition root would resolve it.
///
/// Tests build it directly because that is what the daemon does: the domain
/// receives an already-resolved context and never looks for one (ADR 030).
pub fn central_authority(transaction_id: TransactionId) -> RefundAuthorizationContext {
    RefundAuthorizationContext {
        original_transaction_id: transaction_id,
        authority: RefundAuthority::CentralAccepted,
        authorization_ref: Some(format!("TEST-CENTRAL:{transaction_id}")),
    }
}

/// `(original_line_number, units)` pairs as a refund request.
pub fn refund_lines(lines: &[(i64, i64)]) -> Vec<RefundLineRequest> {
    lines
        .iter()
        .map(|(line_number, units)| RefundLineRequest {
            original_line_number: *line_number,
            quantity: Quantity::from_units(*units).unwrap(),
        })
        .collect()
}

/// A refund request in thousandths, for fractional quantities.
pub fn refund_lines_thousandths(lines: &[(i64, i64)]) -> Vec<RefundLineRequest> {
    lines
        .iter()
        .map(|(line_number, thousandths)| RefundLineRequest {
            original_line_number: *line_number,
            quantity: Quantity::from_thousandths(*thousandths).unwrap(),
        })
        .collect()
}

/// Open a refund with central authority and no fiscal consequence.
pub fn create_refund(
    core: &TestCore,
    transaction_id: TransactionId,
    lines: Vec<RefundLineRequest>,
) -> Result<RefundView, edge_application::AppError> {
    core.create_refund(CreateRefundCommand {
        request_id: None,
        authorization: central_authority(transaction_id),
        lines,
        reason_code: "CUSTOMER_RETURN".to_owned(),
        fiscal_action: RefundFiscalStatus::None,
    })
}

/// A whole refund: open it, take the goods, give the money back.
pub fn full_refund(
    core: &TestCore,
    transaction_id: TransactionId,
    lines: Vec<RefundLineRequest>,
) -> RefundView {
    let refund = create_refund(core, transaction_id, lines).unwrap();
    core.accept_refund(AcceptRefundCommand {
        request_id: None,
        refund_id: refund.refund.refund_id,
    })
    .unwrap();
    core.start_refund_payment(StartRefundPaymentCommand {
        request_id: None,
        refund_id: refund.refund.refund_id,
    })
    .unwrap()
}

/// A finished cash sale, answering with its id so a refund can name it.
pub fn finalized_cash_sale(core: &TestCore, item: AddItemCommand, tendered: i64) -> TransactionId {
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(item).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(core, tendered);
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap()
        .id()
}

/// Arrange an invoiced sale, for the fiscal cases.
///
/// A **fixture**, not a capability: PI.Edge consumes fiscal metadata and has no
/// production operation, protocol message or CLI command that writes it. This
/// reaches the `testing`-only use case to stand in for the upstream system that
/// will eventually supply it (ADR 032).
pub fn mark_invoiced(core: &TestCore, transaction_id: TransactionId) {
    core.mark_transaction_invoiced(MarkTransactionInvoicedCommand {
        request_id: None,
        transaction_id,
        invoice_ref: "INV-0001".to_owned(),
    })
    .unwrap();
}

// -------------------------------------------------------------- hardware

/// A core plus the simulators it drives.
///
/// The test keeps its own handle on each simulator rather than downcasting the
/// port: a trait does not grow an `as_any` so a test can look inside it
/// (CLAUDE.md).
#[derive(Debug)]
pub struct HardwareFixture {
    pub core: TestCore,
    pub printer: Arc<SimulatorPrinter>,
    pub drawer: Arc<SimulatorDrawer>,
}

impl HardwareFixture {
    /// Everything working. A simulator, asked for explicitly (ADR 038).
    pub fn working() -> Self {
        Self::new(SimulatorPrinter::working(), SimulatorDrawer::working())
    }

    /// A core with **no hardware configured**, which is what a misconfigured
    /// production runtime gets.
    pub fn unconfigured() -> TestCore {
        EdgeApplication::with_hardware(
            SqliteEdgeStore::open_in_memory().unwrap(),
            FixedClock::new(start_instant(), 1_000),
            SequentialIdFactory::new(),
            SimulatorPaymentProvider::approving(),
            Hardware::unconfigured(),
        )
    }

    pub fn new(printer: SimulatorPrinter, drawer: SimulatorDrawer) -> Self {
        let printer = Arc::new(printer);
        let drawer = Arc::new(drawer);
        let core = EdgeApplication::with_hardware(
            SqliteEdgeStore::open_in_memory().unwrap(),
            FixedClock::new(start_instant(), 1_000),
            SequentialIdFactory::new(),
            SimulatorPaymentProvider::approving(),
            Hardware::new(Arc::clone(&printer) as _, Arc::clone(&drawer) as _, 48),
        );
        Self {
            core,
            printer,
            drawer,
        }
    }

    /// A till ready to sell, with stock.
    pub fn open_till(&self) -> &TestCore {
        open_register_with_float(&self.core, 100_000);
        bootstrap_stock(&self.core, &[("SKU-001", 10)]);
        &self.core
    }

    pub fn opens(&self) -> usize {
        self.drawer.open_count()
    }

    pub fn jobs(&self) -> usize {
        self.printer.job_count()
    }
}

/// A completed scan, as a host's parser would produce it.
pub fn scan_of(barcode: &str) -> SubmitScanCommand {
    scan_with(uuid::Uuid::new_v4(), barcode)
}

/// A scan with a chosen event id, for the replay cases.
pub fn scan_with(event_id: uuid::Uuid, barcode: &str) -> SubmitScanCommand {
    SubmitScanCommand {
        scan: ScanEvent {
            event_id,
            barcode: barcode.to_owned(),
            occurred_at: Timestamp::from_datetime(start_instant()),
            device_id: "scanner".to_owned(),
        },
    }
}

/// A supervisor identity, distinct from the demo cashier.
pub fn supervisor() -> OperatorId {
    OperatorId::new("SUPERVISOR-01").unwrap()
}

/// A lifetime that believes it is two days later, for business-day identity.
pub fn core_two_days_later(path: &std::path::Path) -> TestCore {
    let instant = start_instant() + chrono::Duration::days(2);
    EdgeApplication::with_hardware(
        SqliteEdgeStore::open(path).unwrap(),
        FixedClock::new(instant, 1_000),
        SequentialIdFactory::starting_at(5_000),
        SimulatorPaymentProvider::approving(),
        Hardware::simulated(),
    )
}

// ------------------------------------------------------------------- sync

/// A till that syncs through a scripted simulator, plus the handle to script
/// it and to read what it received.
///
/// The simulator is asked for **explicitly**, here as everywhere: a core built
/// without one has no transport at all and answers `SYNC_NOT_CONFIGURED`
/// (ADR 040).
#[derive(Debug)]
pub struct SyncFixture {
    pub core: TestCore,
    pub transport: Arc<SimulatorSyncTransport>,
}

impl SyncFixture {
    pub fn in_memory() -> Self {
        Self::build(SqliteEdgeStore::open_in_memory().unwrap(), 0, 0)
    }

    /// A till on a real file, so it can be closed and reopened.
    pub fn at(path: &std::path::Path) -> Self {
        Self::build(SqliteEdgeStore::open(path).unwrap(), 0, 0)
    }

    /// The same installation, reopened as a fresh runtime `days` later.
    ///
    /// A new id range, because two runtimes minting the same ids would be a
    /// fiction no real pair of processes produces.
    pub fn reopen(path: &std::path::Path, days: i64, ids_from: u64) -> Self {
        Self::build(SqliteEdgeStore::open(path).unwrap(), days, ids_from)
    }

    /// A till whose card provider follows a script, for the refund that comes
    /// back unresolved and sits in `FINANCIAL_PENDING`.
    pub fn scripted(script: SimulatorScript) -> ScriptedSyncFixture {
        let transport = Arc::new(SimulatorSyncTransport::new());
        let core = EdgeApplication::with_hardware(
            SqliteEdgeStore::open_in_memory().unwrap(),
            FixedClock::new(start_instant(), 1_000),
            SequentialIdFactory::new(),
            SimulatorPaymentProvider::new(script),
            Hardware::simulated(),
        )
        .with_sync(SyncRuntime::with_transport(Arc::clone(&transport) as _));
        ScriptedSyncFixture { core, transport }
    }

    fn build(store: SqliteEdgeStore, days: i64, ids_from: u64) -> Self {
        let transport = Arc::new(SimulatorSyncTransport::new());
        let instant = start_instant() + chrono::Duration::days(days);
        let ids = if ids_from == 0 {
            SequentialIdFactory::new()
        } else {
            SequentialIdFactory::starting_at(ids_from)
        };
        let core = EdgeApplication::with_hardware(
            store,
            FixedClock::new(instant, 1_000),
            ids,
            SimulatorPaymentProvider::approving(),
            Hardware::simulated(),
        )
        .with_sync(SyncRuntime::with_transport(Arc::clone(&transport) as _));
        Self { core, transport }
    }

    /// The same till, dispatching in smaller batches.
    pub fn with_batch_limit(self, limit: usize) -> Self {
        let transport = self.transport;
        Self {
            core: self.core.with_sync(
                SyncRuntime::with_transport(Arc::clone(&transport) as _).with_batch_limit(limit),
            ),
            transport,
        }
    }

    pub fn counts(&self) -> OutboxCounts {
        self.core.get_sync_status().unwrap().outbox
    }

    /// Every fact type the outbox holds, oldest `seq` first.
    pub fn fact_types(&self) -> Vec<String> {
        let mut types = Vec::new();
        for status in [
            OutboxStatus::Pending,
            OutboxStatus::Sending,
            OutboxStatus::Sent,
            OutboxStatus::Failed,
        ] {
            types.extend(
                outbox_entries(&self.core, status)
                    .into_iter()
                    .map(|entry| (entry.seq, entry.event.event_type.as_str().to_owned())),
            );
        }
        types.sort();
        types.into_iter().map(|(_, kind)| kind).collect()
    }
}

/// A till on a file path whose printer answers however the test says.
///
/// Needed to span two business days with different hardware: the first day
/// ends with an unresolved receipt, the second one prints cleanly.
pub fn core_at_with_printer(
    path: &std::path::Path,
    days: i64,
    ids_from: u64,
    printer: SimulatorPrinter,
) -> TestCore {
    EdgeApplication::with_hardware(
        SqliteEdgeStore::open(path).unwrap(),
        FixedClock::new(start_instant() + chrono::Duration::days(days), 1_000),
        SequentialIdFactory::starting_at(ids_from),
        SimulatorPaymentProvider::approving(),
        Hardware::new(
            Arc::new(printer) as _,
            Arc::new(SimulatorDrawer::working()) as _,
            48,
        ),
    )
}

/// A till with **no sync transport**, which is the production default.
pub fn offline_core_at(path: &std::path::Path) -> TestCore {
    core_with_store(SqliteEdgeStore::open(path).unwrap())
}

/// The outbox rows in one state, oldest first.
pub fn outbox_entries(core: &TestCore, status: OutboxStatus) -> Vec<OutboxEntry> {
    core.store()
        .transact(|uow| uow.list_outbox_by_status(status, 500))
        .unwrap()
}

/// The event ids of the facts in one state, oldest first.
pub fn outbox_ids(core: &TestCore, status: OutboxStatus) -> Vec<EventId> {
    outbox_entries(core, status)
        .into_iter()
        .map(|entry| entry.event.event_id)
        .collect()
}

/// Claim facts and stop, which is what a runtime that died mid-dispatch left
/// behind: the claim committed, the answer never came.
pub fn claim_and_abandon(core: &TestCore, event_ids: &[EventId]) -> Vec<EventId> {
    let now = core.clock_now();
    core.store()
        .transact(|uow| uow.claim_outbox(event_ids, now))
        .unwrap()
}

/// The same thing with a scripted card provider.
#[derive(Debug)]
pub struct ScriptedSyncFixture {
    pub core: ScriptedCore,
    pub transport: Arc<SimulatorSyncTransport>,
}

impl ScriptedSyncFixture {
    /// Every fact the outbox holds, oldest `seq` first, as `(type, version)`.
    pub fn facts(&self) -> Vec<(String, i64)> {
        scripted_outbox_facts(&self.core)
    }

    /// Only the refund facts, so an assertion is not coupled to the sale
    /// aggregate's own row version.
    pub fn refund_facts(&self) -> Vec<(String, i64)> {
        self.facts()
            .into_iter()
            .filter(|(kind, _)| kind.starts_with("REFUND_"))
            .collect()
    }
}

/// Every outbox fact of a scripted core, oldest `seq` first.
pub fn scripted_outbox_facts(core: &ScriptedCore) -> Vec<(String, i64)> {
    let mut rows = Vec::new();
    for status in [
        OutboxStatus::Pending,
        OutboxStatus::Sending,
        OutboxStatus::Sent,
        OutboxStatus::Failed,
    ] {
        let entries = core
            .store()
            .transact(|uow| uow.list_outbox_by_status(status, 500))
            .unwrap();
        rows.extend(entries.into_iter().map(|entry| {
            (
                entry.seq,
                entry.event.event_type.as_str().to_owned(),
                entry.event.aggregate_version,
            )
        }));
    }
    rows.sort();
    rows.into_iter()
        .map(|(_, kind, version)| (kind, version))
        .collect()
}
