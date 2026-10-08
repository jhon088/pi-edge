//! Shared fixtures for the PI.Edge integration tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::{DateTime, Utc};
use edge_application::commands::*;
use edge_application::testing::{FixedClock, SequentialIdFactory};
use edge_application::EdgeApplication;
use edge_domain::prelude::*;
use edge_storage::SqliteEdgeStore;

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
    core.init_device(InitDeviceCommand {
        tenant_id: TenantId::new("TENANT-DEMO").unwrap(),
        store_id: StoreId::new("STORE-DEMO").unwrap(),
        currency: Currency::Mxn,
    })
    .unwrap();
}

/// Device + open business day + open session on `REG-001`.
pub fn open_register(core: &TestCore) {
    init_device(core);
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

/// A single-line item command: 100.00 MXN at 16% tax.
pub fn demo_item() -> AddItemCommand {
    item("SKU-001", "Producto prueba", 10_000, 1_600, 1)
}

pub fn item(
    product: &str,
    description: &str,
    price_minor: i64,
    tax_rate_bps: u32,
    qty_units: i64,
) -> AddItemCommand {
    AddItemCommand {
        request_id: None,
        product_ref: ProductRef::new(product).unwrap(),
        sku: Some(product.to_owned()),
        barcode: None,
        description: description.to_owned(),
        quantity: Quantity::from_units(qty_units).unwrap(),
        unit_price_minor: price_minor,
        tax_rate: TaxRate::from_bps(tax_rate_bps).unwrap(),
        discount_minor: 0,
    }
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
