//! Fixtures for the crash-recovery suite.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use chrono::{DateTime, Utc};
use edge_application::commands::*;
use edge_application::testing::{CatalogFixture, FixedClock, SequentialIdFactory, DEMO_BARCODE};
use edge_application::EdgeApplication;
use edge_catalog::{Barcode, ProductSelector};
use edge_domain::prelude::*;
use edge_storage::SqliteEdgeStore;

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
    EdgeApplication::new(
        SqliteEdgeStore::open(path).unwrap(),
        FixedClock::new(start_instant(), 1_000),
        SequentialIdFactory::starting_at(id_offset),
    )
}

/// Same, with the clock frozen at an arbitrary instant: a later "lifetime"
/// that believes it is running on another day.
pub fn core_at_instant(path: &Path, id_offset: u64, instant: &str) -> TestCore {
    let instant = DateTime::parse_from_rfc3339(instant)
        .unwrap()
        .with_timezone(&Utc);
    EdgeApplication::new(
        SqliteEdgeStore::open(path).unwrap(),
        FixedClock::frozen(instant),
        SequentialIdFactory::starting_at(id_offset),
    )
}

pub fn open_register(core: &TestCore) {
    core.init_device(InitDeviceCommand {
        tenant_id: TenantId::new("TENANT-DEMO").unwrap(),
        store_id: StoreId::new("STORE-DEMO").unwrap(),
        currency: Currency::Mxn,
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

pub fn demo_item() -> AddItemCommand {
    AddItemCommand {
        request_id: None,
        selector: ProductSelector::Barcode(Barcode::new(DEMO_BARCODE).unwrap()),
        quantity: Quantity::from_units(1).unwrap(),
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
