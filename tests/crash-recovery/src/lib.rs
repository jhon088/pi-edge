//! Fixtures for the crash-recovery suite.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use chrono::{DateTime, Utc};
use edge_application::commands::*;
use edge_application::testing::{CatalogFixture, FixedClock, SequentialIdFactory, DEMO_BARCODE};
use edge_application::EdgeApplication;
use edge_catalog::{Barcode, ProductSelector};
use edge_domain::prelude::*;
use edge_inventory::BootstrapItem;
use edge_payments::{SimulatorPaymentProvider, SimulatorScript, TenderType};
use edge_refunds::{RefundAuthority, RefundAuthorizationContext, RefundLineRequest};
use edge_storage::SqliteEdgeStore;
use std::sync::Arc;

use edge_application::hardware_service::Hardware;
use edge_hardware::{SimulatorDrawer, SimulatorPrinter};

pub use edge_payments::TenderType as Tender;

pub type TestCore = EdgeApplication<SqliteEdgeStore, FixedClock, SequentialIdFactory>;

pub const START: &str = "2026-10-08T15:00:00Z";

fn start_instant() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(START)
        .unwrap()
        .with_timezone(&Utc)
}

/// Build a core over a database file. Called twice on the same path to
/// simulate a restart: a brand new application object, same durable state.
///
/// `id_offset` keeps generated ids distinct across "process lifetimes".
pub fn core_at(path: &Path, id_offset: u64) -> TestCore {
    // Simulators are asked for explicitly (ADR 038).
    EdgeApplication::with_hardware(
        SqliteEdgeStore::open(path).unwrap(),
        FixedClock::new(start_instant(), 1_000),
        SequentialIdFactory::starting_at(id_offset),
        SimulatorPaymentProvider::approving(),
        Hardware::simulated(),
    )
}

/// Build a core whose asset store lives in `assets/` next to the database, so
/// a "restart" rediscovers both from the same directory.
pub fn core_at_with_assets(path: &Path, id_offset: u64) -> TestCore {
    let root = path.parent().unwrap().join("assets");
    core_at(path, id_offset).with_assets(Arc::new(
        edge_assets::FileSystemAssetStore::open(root).unwrap(),
    ))
}

/// The bytes of a tiny valid PNG, and the `AssetRef` they answer to.
pub fn png_asset(tag: u8, length: usize) -> (Vec<u8>, edge_catalog::AssetRef) {
    use sha2::{Digest, Sha256};
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend((0..length).map(|index| (index as u8).wrapping_mul(tag).wrapping_add(tag)));
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let mut hex = String::new();
    for byte in hasher.finalize() {
        hex.push_str(&format!("{byte:02x}"));
    }
    let asset = edge_catalog::AssetRef::import(
        format!("sha256:{hex}"),
        "image/png",
        i64::try_from(bytes.len()).unwrap(),
    )
    .unwrap();
    (bytes, asset)
}

/// Same, with the clock frozen at an arbitrary instant: a later "lifetime"
/// that believes it is running on another day.
pub fn core_at_instant(path: &Path, id_offset: u64, instant: &str) -> TestCore {
    let instant = DateTime::parse_from_rfc3339(instant)
        .unwrap()
        .with_timezone(&Utc);
    EdgeApplication::with_hardware(
        SqliteEdgeStore::open(path).unwrap(),
        FixedClock::frozen(instant),
        SequentialIdFactory::starting_at(id_offset),
        SimulatorPaymentProvider::approving(),
        Hardware::simulated(),
    )
}

pub fn open_register(core: &TestCore) {
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

/// Import an opening position: `(product_ref, units)` pairs.
pub fn bootstrap_stock(core: &TestCore, items: &[(&str, i64)]) {
    core.import_inventory_bootstrap(bootstrap_command("WMS-SNAPSHOT-1", items))
        .unwrap();
}

/// The command a bootstrap import would arrive as, so a test can replay it or
/// arm a crash around it.
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

/// On hand on a scripted core.
pub fn on_hand_scripted(core: &ScriptedCore, product_ref: &str) -> i64 {
    core.get_inventory_position(ProductRef::new(product_ref).unwrap(), 100)
        .unwrap()
        .position
        .on_hand_thousandths
}

/// Import an opening position on a scripted core.
pub fn bootstrap_stock_scripted(core: &ScriptedCore, items: &[(&str, i64)]) {
    core.import_inventory_bootstrap(bootstrap_command("WMS-SNAPSHOT-1", items))
        .unwrap();
}

/// On hand in thousandths, straight from the core's own fold.
pub fn on_hand(core: &TestCore, product_ref: &str) -> i64 {
    core.get_inventory_position(ProductRef::new(product_ref).unwrap(), 100)
        .unwrap()
        .position
        .on_hand_thousandths
}

/// Provision the device identity and nothing else: the asset tests have no
/// sale to make, and a catalog they cannot activate yet.
pub fn init_device(core: &TestCore) {
    core.init_device(InitDeviceCommand {
        tenant_id: TenantId::new("TENANT-DEMO").unwrap(),
        store_id: StoreId::new("STORE-DEMO").unwrap(),
        currency: Currency::Mxn,
        allow_negative_inventory: true,
    })
    .unwrap();
}

/// Every movement of one product, oldest first.
pub fn movements(core: &TestCore, product_ref: &str) -> Vec<edge_inventory::InventoryMovement> {
    core.get_inventory_movements(ProductRef::new(product_ref).unwrap(), 100, 0)
        .unwrap()
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

/// A finished card sale of one demo unit, on a scripted core.
pub fn card_sale(core: &ScriptedCore) -> TransactionId {
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Card,
        amount_minor: totalized.state().totals.grand_total.minor(),
    })
    .unwrap();
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap()
        .id()
}

// -------------------------------------------------------------- hardware

/// A completed scan, as a host's parser would produce it.
pub fn scan_of(barcode: &str) -> SubmitScanCommand {
    SubmitScanCommand {
        scan: edge_hardware::ScanEvent {
            event_id: uuid::Uuid::new_v4(),
            barcode: barcode.to_owned(),
            occurred_at: Timestamp::from_datetime(start_instant()),
            device_id: "scanner".to_owned(),
        },
    }
}

/// A core over a database file, with simulators whose counters the test keeps.
///
/// Two lifetimes over one database get *fresh* simulators, which is the point:
/// a drawer's open count cannot survive a process, so whatever stops a second
/// opening has to be durable (ADR 037).
#[derive(Debug)]
pub struct HardwareLifetime {
    pub core: TestCore,
    pub printer: Arc<SimulatorPrinter>,
    pub drawer: Arc<SimulatorDrawer>,
}

impl HardwareLifetime {
    pub fn open(path: &Path, id_offset: u64) -> Self {
        Self::with(path, id_offset, SimulatorDrawer::working())
    }

    pub fn with(path: &Path, id_offset: u64, drawer: SimulatorDrawer) -> Self {
        let printer = Arc::new(SimulatorPrinter::working());
        let drawer = Arc::new(drawer);
        let core = EdgeApplication::with_hardware(
            SqliteEdgeStore::open(path).unwrap(),
            FixedClock::new(start_instant(), 1_000),
            SequentialIdFactory::starting_at(id_offset),
            SimulatorPaymentProvider::approving(),
            Hardware::new(Arc::clone(&printer) as _, Arc::clone(&drawer) as _, 48),
        );
        Self {
            core,
            printer,
            drawer,
        }
    }

    pub fn opens(&self) -> usize {
        self.drawer.open_count()
    }
}

// --------------------------------------------------------------- refunds

/// The authority a refund needs, as the composition root would resolve it.
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

/// A finished cash sale, answering with its id so a refund can name it.
pub fn finalized_cash_sale(core: &TestCore, tendered_minor: i64) -> TransactionId {
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(core, tendered_minor);
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap()
        .id()
}

pub fn demo_item() -> AddItemCommand {
    AddItemCommand {
        request_id: None,
        selector: ProductSelector::Barcode(Barcode::new(DEMO_BARCODE).unwrap()),
        quantity: Quantity::from_units(1).unwrap(),
        selections: Vec::new(),
        preparation_note: None,
    }
}

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

pub type ScriptedCore =
    EdgeApplication<SqliteEdgeStore, FixedClock, SequentialIdFactory, SimulatorPaymentProvider>;

/// A lifetime whose card provider follows a script, so a lost provider call is
/// reproducible rather than a matter of timing.
pub fn scripted_core_at(path: &Path, id_offset: u64, script: SimulatorScript) -> ScriptedCore {
    EdgeApplication::with_hardware(
        SqliteEdgeStore::open(path).unwrap(),
        FixedClock::new(start_instant(), 1_000),
        SequentialIdFactory::starting_at(id_offset),
        SimulatorPaymentProvider::new(script),
        Hardware::simulated(),
    )
}

/// Open a register with an explicit float, for cash recovery cases.
pub fn open_register_with_float(core: &TestCore, opening_cash_minor: i64) {
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
        opening_cash_minor,
    })
    .unwrap();
}

/// A finished cash sale of the demo product: 116.00, tendered `tendered_minor`.
pub fn cash_sale(core: &TestCore, tendered_minor: i64) {
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(AddItemCommand {
        request_id: None,
        selector: ProductSelector::Barcode(Barcode::new(DEMO_BARCODE).unwrap()),
        quantity: Quantity::from_units(1).unwrap(),
        selections: Vec::new(),
        preparation_note: None,
    })
    .unwrap();
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

// -------------------------------------------------------------------- sync

/// A core on `path` that syncs through a scripted simulator.
///
/// The simulator is asked for explicitly; a core built by `core_at` has no
/// transport at all (ADR 040).
pub fn syncing_core_at(
    path: &Path,
    id_offset: u64,
    transport: &Arc<edge_sync::simulator::SimulatorSyncTransport>,
) -> TestCore {
    core_at(path, id_offset).with_sync(edge_application::sync_service::SyncRuntime::with_transport(
        Arc::clone(transport) as _,
    ))
}

/// The outbox rows in one state, oldest first.
pub fn outbox_entries(core: &TestCore, status: OutboxStatus) -> Vec<OutboxEntry> {
    use edge_application::ports::EdgeStore;
    core.store()
        .transact(|uow| uow.list_outbox_by_status(status, 500))
        .unwrap()
}

// ------------------------------------------------------------------ receipts

/// A core on `path` with a printer that answers however the test says.
///
/// A simulator, asked for explicitly (ADR 038).
pub fn printing_core_at(
    path: &Path,
    id_offset: u64,
    printer: SimulatorPrinter,
) -> (TestCore, Arc<SimulatorPrinter>) {
    let printer = Arc::new(printer);
    let core = EdgeApplication::with_hardware(
        SqliteEdgeStore::open(path).unwrap(),
        FixedClock::new(start_instant(), 1_000),
        SequentialIdFactory::starting_at(id_offset),
        SimulatorPaymentProvider::approving(),
        Hardware::new(
            Arc::clone(&printer) as _,
            Arc::new(SimulatorDrawer::working()) as _,
            48,
        ),
    );
    (core, printer)
}
