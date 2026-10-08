//! Golden case harness.
//!
//! Time and ids are deterministic (frozen clock, sequential UUIDs), so the
//! protocol output of a known sale is byte-stable. Regenerate intentionally
//! with `UPDATE_GOLDEN=1 cargo test -p edge-golden-tests`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use edge_application::commands::*;
use edge_application::testing::{
    CatalogFixture, FixedClock, FixturePrice, FixtureProduct, SequentialIdFactory,
};
use edge_application::EdgeApplication;
use edge_catalog::{Barcode, ProductSelector};
use edge_domain::prelude::*;
use edge_storage::SqliteEdgeStore;
use serde_json::Value;

pub type GoldenCore = EdgeApplication<SqliteEdgeStore, FixedClock, SequentialIdFactory>;

pub const FROZEN_AT: &str = "2026-10-08T15:00:00Z";

/// Core with frozen time and sequential ids.
pub fn golden_core() -> GoldenCore {
    golden_core_at(FROZEN_AT)
}

/// Core frozen at an arbitrary instant, for pricing schedules.
pub fn golden_core_at(instant: &str) -> GoldenCore {
    let instant: DateTime<Utc> = DateTime::parse_from_rfc3339(instant)
        .unwrap()
        .with_timezone(&Utc);
    EdgeApplication::new(
        SqliteEdgeStore::open_in_memory().unwrap(),
        FixedClock::frozen(instant),
        SequentialIdFactory::new(),
    )
}

/// When the scheduled price in the golden catalog takes over.
pub const SCHEDULED_FROM: &str = "2026-10-15T00:00:00Z";

fn scheduled_from() -> Timestamp {
    Timestamp::parse_rfc3339(SCHEDULED_FROM).unwrap()
}

/// Import and activate a catalog, then open the register.
pub fn open_register_with(core: &GoldenCore, catalog: &CatalogFixture) {
    core.init_device(InitDeviceCommand {
        tenant_id: TenantId::new("TENANT-DEMO").unwrap(),
        store_id: StoreId::new("STORE-DEMO").unwrap(),
        currency: Currency::Mxn,
    })
    .unwrap();
    catalog.activate(core).unwrap();
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

/// Catalog used by the golden cases: one product per tax rate, both price
/// modes, prices chosen so the arithmetic cannot be exact, a store override and
/// a scheduled change.
pub fn golden_catalog(version: i64) -> CatalogFixture {
    CatalogFixture::new(version)
        .with_store_book(STORE_BOOK, "STORE-DEMO", 0)
        .with(
            FixtureProduct::new("SKU-16", "Gravado 16%", 10_000)
                .sku("SKU-16")
                .barcode(BARCODE_16),
        )
        .with(
            FixtureProduct::new("SKU-8", "Gravado 8%", 10_000)
                .sku("SKU-8")
                .barcode(BARCODE_8)
                .tax_profile("IVA8"),
        )
        .with(
            FixtureProduct::new("SKU-0", "Tasa 0%", 1_999)
                .sku("SKU-0")
                .barcode(BARCODE_0)
                .tax_profile("IVA0"),
        )
        .with(
            FixtureProduct::new("SKU-INC-16", "Precio con IVA 16%", 0)
                .sku("SKU-INC-16")
                .barcode(BARCODE_INCLUSIVE_16)
                .priced(FixturePrice::base(11_600).inclusive()),
        )
        .with(
            FixtureProduct::new("SKU-INC-8", "Precio con IVA 8%", 0)
                .sku("SKU-INC-8")
                .barcode(BARCODE_INCLUSIVE_8)
                .tax_profile("IVA8")
                .priced(FixturePrice::base(10_800).inclusive()),
        )
        .with(
            FixtureProduct::new("SKU-INC-0", "Precio sin impuesto", 0)
                .sku("SKU-INC-0")
                .barcode(BARCODE_INCLUSIVE_0)
                .tax_profile("IVA0")
                .priced(FixturePrice::base(9_999).inclusive()),
        )
        .with(
            FixtureProduct::new("SKU-FRACTIONAL", "Granel 16%", 333)
                .sku("SKU-FRACTIONAL")
                .barcode(BARCODE_FRACTIONAL),
        )
        .with(
            FixtureProduct::new("SKU-ODD-INC", "Centavo impar con IVA", 0)
                .sku("SKU-ODD-INC")
                .barcode(BARCODE_ODD_INCLUSIVE)
                .priced(FixturePrice::base(99).inclusive()),
        )
        // Same product, cheaper in this store's own book.
        .with(
            FixtureProduct::new("SKU-STORE", "Oferta de tienda", 10_000)
                .sku("SKU-STORE")
                .barcode(BARCODE_STORE_OVERRIDE)
                .plus_price(FixturePrice::base(8_900).in_book(STORE_BOOK)),
        )
        // A price change that starts later.
        .with(
            FixtureProduct::new("SKU-SCHEDULED", "Precio programado", 0)
                .sku("SKU-SCHEDULED")
                .barcode(BARCODE_SCHEDULED)
                .priced(FixturePrice::base(10_000).until(scheduled_from()))
                .plus_price(FixturePrice::base(12_000).from(scheduled_from())),
        )
}

/// Price book scoped to the demo store.
pub const STORE_BOOK: &str = "STORE-DEMO-BOOK";

pub const BARCODE_16: &str = "7501000000016";
pub const BARCODE_8: &str = "7501000000008";
pub const BARCODE_0: &str = "7501000000000";
pub const BARCODE_INCLUSIVE_16: &str = "7501000000116";
pub const BARCODE_INCLUSIVE_8: &str = "7501000000108";
pub const BARCODE_INCLUSIVE_0: &str = "7501000000100";
pub const BARCODE_FRACTIONAL: &str = "7501000000333";
pub const BARCODE_ODD_INCLUSIVE: &str = "7501000000099";
pub const BARCODE_STORE_OVERRIDE: &str = "7501000000777";
pub const BARCODE_SCHEDULED: &str = "7501000000888";

/// Scan a barcode with a quantity in thousandths.
pub fn scan(core: &GoldenCore, barcode: &str, quantity_thousandths: i64) {
    core.add_item(AddItemCommand {
        request_id: None,
        selector: ProductSelector::Barcode(Barcode::new(barcode).unwrap()),
        quantity: Quantity::from_thousandths(quantity_thousandths).unwrap(),
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
