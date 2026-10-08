//! Golden case harness.
//!
//! Time and ids are deterministic (frozen clock, sequential UUIDs), so the
//! protocol output of a known sale is byte-stable. Regenerate intentionally
//! with `UPDATE_GOLDEN=1 cargo test -p edge-golden-tests`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use edge_application::commands::*;
use edge_application::testing::{FixedClock, SequentialIdFactory};
use edge_application::EdgeApplication;
use edge_domain::prelude::*;
use edge_storage::SqliteEdgeStore;
use serde_json::Value;

pub type GoldenCore = EdgeApplication<SqliteEdgeStore, FixedClock, SequentialIdFactory>;

pub const FROZEN_AT: &str = "2026-10-08T15:00:00Z";

/// Core with frozen time and sequential ids.
pub fn golden_core() -> GoldenCore {
    let instant: DateTime<Utc> = DateTime::parse_from_rfc3339(FROZEN_AT)
        .unwrap()
        .with_timezone(&Utc);
    EdgeApplication::new(
        SqliteEdgeStore::open_in_memory().unwrap(),
        FixedClock::frozen(instant),
        SequentialIdFactory::new(),
    )
}

pub fn open_register(core: &GoldenCore) {
    core.init_device(InitDeviceCommand {
        tenant_id: TenantId::new("TENANT-DEMO").unwrap(),
        store_id: StoreId::new("STORE-DEMO").unwrap(),
        currency: Currency::Mxn,
    })
    .unwrap();
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

#[derive(Debug)]
pub struct ItemSpec {
    pub product: &'static str,
    pub description: &'static str,
    pub price_minor: i64,
    pub tax_rate_bps: u32,
    pub quantity_thousandths: i64,
    pub discount_minor: i64,
}

pub fn add_item(core: &GoldenCore, spec: ItemSpec) {
    core.add_item(AddItemCommand {
        request_id: None,
        product_ref: ProductRef::new(spec.product).unwrap(),
        sku: Some(spec.product.to_owned()),
        barcode: None,
        description: spec.description.to_owned(),
        quantity: Quantity::from_thousandths(spec.quantity_thousandths).unwrap(),
        unit_price_minor: spec.price_minor,
        tax_rate: TaxRate::from_bps(spec.tax_rate_bps).unwrap(),
        discount_minor: spec.discount_minor,
    })
    .unwrap();
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(format!("{name}.json"))
}

/// Compare a protocol value against its recorded golden file.
pub fn assert_golden(name: &str, value: &Value) {
    let path = fixture_path(name);
    let mut actual = serde_json::to_string_pretty(value).unwrap();
    actual.push('\n');

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual.as_bytes()).unwrap();
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "missing golden file {}: {error}. Regenerate with UPDATE_GOLDEN=1",
            path.display()
        )
    });
    assert_eq!(
        actual, expected,
        "golden mismatch for `{name}`; if this change is intended, regenerate with UPDATE_GOLDEN=1"
    );
}
