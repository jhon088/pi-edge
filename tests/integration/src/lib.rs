//! Shared fixtures for the PI.Edge integration tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::{DateTime, Utc};
use edge_application::commands::*;
use edge_application::ports::InventoryBootstrapState;
use edge_application::queries::InventoryPositionView;
use edge_application::testing::{
    CatalogFixture, FixedClock, FixtureProduct, SequentialIdFactory, DEMO_BARCODE,
};
use edge_application::EdgeApplication;
use edge_catalog::{Barcode, ProductSelector};
use edge_domain::prelude::*;
use edge_inventory::BootstrapItem;
use edge_payments::{SimulatorPaymentProvider, SimulatorScript, TenderType};
use edge_storage::SqliteEdgeStore;

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
    EdgeApplication::new(
        store,
        FixedClock::new(start_instant(), 1_000),
        SequentialIdFactory::new(),
    )
}

pub fn in_memory_core() -> TestCore {
    core_with_store(SqliteEdgeStore::open_in_memory().unwrap())
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
    }
}

/// Add an item by canonical product reference.
pub fn item(product_ref: &str, qty_units: i64) -> AddItemCommand {
    AddItemCommand {
        request_id: None,
        selector: ProductSelector::ProductRef(ProductRef::new(product_ref).unwrap()),
        quantity: Quantity::from_units(qty_units).unwrap(),
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
    EdgeApplication::with_payment_provider(
        SqliteEdgeStore::open_in_memory().unwrap(),
        FixedClock::new(start_instant(), 1_000),
        SequentialIdFactory::new(),
        SimulatorPaymentProvider::new(script),
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

/// A supervisor identity, distinct from the demo cashier.
pub fn supervisor() -> OperatorId {
    OperatorId::new("SUPERVISOR-01").unwrap()
}

/// A lifetime that believes it is two days later, for business-day identity.
pub fn core_two_days_later(path: &std::path::Path) -> TestCore {
    let instant = start_instant() + chrono::Duration::days(2);
    EdgeApplication::new(
        SqliteEdgeStore::open(path).unwrap(),
        FixedClock::new(instant, 1_000),
        SequentialIdFactory::starting_at(5_000),
    )
}
