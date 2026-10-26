//! Golden case harness.
//!
//! Time and ids are deterministic (frozen clock, sequential UUIDs), so the
//! protocol output of a known sale is byte-stable. Regenerate intentionally
//! with `UPDATE_GOLDEN=1 cargo test -p edge-golden-tests`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::borrow::Cow;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use edge_application::commands::*;
use edge_application::hardware_service::Hardware;
use edge_application::ports::InventoryBootstrapState;
use edge_application::queries::{InventoryPositionView, RefundView};
use edge_application::testing::{
    CatalogFixture, FixedClock, FixturePrice, FixtureProduct, SequentialIdFactory,
};
use edge_application::EdgeApplication;
use edge_catalog::promotions::{
    NewPromotion, ProductApplicability, Promotion, PromotionRule, Stackability,
};
use edge_catalog::scope::CatalogScope;
use edge_catalog::{Barcode, ProductSelector};
use edge_domain::prelude::*;
use edge_inventory::BootstrapItem;
use edge_payments::{SimulatorPaymentProvider, SimulatorScript, TenderType};
use edge_refunds::{
    RefundAuthority, RefundAuthorizationContext, RefundFiscalStatus, RefundLineRequest,
};
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
        allow_negative_inventory: true,
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
        selections: Vec::new(),
        preparation_note: None,
    })
    .unwrap();
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(format!("{name}.json"))
}

/// Compare a protocol value against its recorded golden file.
/// Line endings, and nothing else, made the same on both sides.
///
/// A golden file is a **text** snapshot, so the comparison stays textual:
/// whitespace, field order, formatting and values all still have to match
/// exactly. The one thing that must not count is how the checkout wrote the
/// line separator — git on Windows hands back CRLF for a file committed with
/// LF, which made every golden test fail there on payloads that were
/// identical.
///
/// Applied to both sides, so this can never hide a difference that is really
/// in the data: a `\r\n` in the *value* normalises on both sides too, and
/// any other byte is still compared as written.
fn normalize_line_endings(value: &str) -> Cow<'_, str> {
    if !value.contains('\r') {
        return Cow::Borrowed(value);
    }
    Cow::Owned(value.replace("\r\n", "\n").replace('\r', "\n"))
}

pub fn assert_golden(name: &str, value: &Value) {
    let path = fixture_path(name);
    let mut actual = serde_json::to_string_pretty(value).unwrap();
    actual.push('\n');

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        // Canonical LF on every platform: a regenerated golden must not
        // depend on which machine regenerated it.
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, normalize_line_endings(&actual).as_bytes()).unwrap();
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "missing golden file {}: {error}. Regenerate with UPDATE_GOLDEN=1",
            path.display()
        )
    });
    assert_eq!(
        normalize_line_endings(&actual),
        normalize_line_endings(&expected),
        "golden mismatch for `{name}`; if this change is intended, regenerate with UPDATE_GOLDEN=1"
    );
}

// ------------------------------------------------------------- promotions

/// Promotional catalog: the same products as [`golden_catalog`] plus the four
/// rule kinds, so every golden promotion case prices from a real snapshot.
///
/// The promotions are *all* in force at [`FROZEN_AT`]; which ones actually
/// apply is decided by each case's basket, never by the fixture.
pub fn promotional_catalog(version: i64) -> CatalogFixture {
    golden_catalog(version)
        // 10% off one product.
        .promoting(percentage_off(
            "PROMO-PCT-16",
            10,
            Stackability::Stackable,
            Some("SKU-16"),
            1_000,
        ))
        // 15.00 off one product, lower priority so it stacks after the above.
        .promoting(fixed_off(
            "PROMO-FIX-16",
            5,
            Stackability::Stackable,
            Some("SKU-16"),
            1_500,
        ))
        // Buy 2 get 1 on the 8% product.
        .promoting(buy_n_get_m(
            "PROMO-BOGO-8",
            0,
            Stackability::Stackable,
            Some("SKU-8"),
            2,
            1,
        ))
        // 20% off the tax-inclusive product: the inclusive path with a
        // discount.
        .promoting(percentage_off(
            "PROMO-PCT-INC",
            0,
            Stackability::Stackable,
            Some("SKU-INC-16"),
            2_000,
        ))
        // Exclusive 30% off the store-override product: it must shut out the
        // basket-wide one below.
        .promoting(percentage_off(
            "PROMO-EXC-STORE",
            100,
            Stackability::Exclusive,
            Some("SKU-STORE"),
            3_000,
        ))
        // 50.00 off the basket once it reaches 500.00.
        .promoting(threshold("PROMO-THRESHOLD", 0, 50_000, 5_000))
}

pub fn promotion(
    id: &str,
    priority: i32,
    stackability: Stackability,
    rule: PromotionRule,
) -> Promotion {
    Promotion::import(NewPromotion {
        promotion_id: PromotionId::new(id).unwrap(),
        code: id.to_owned(),
        name: format!("Promoción {id}"),
        active: true,
        priority,
        stackability,
        scope: CatalogScope::Tenant,
        valid_from: None,
        valid_until: None,
        rule,
    })
    .unwrap()
}

fn applies_to(product_ref: Option<&str>) -> ProductApplicability {
    match product_ref {
        Some(value) => ProductApplicability::Product {
            product_ref: ProductRef::new(value).unwrap(),
        },
        None => ProductApplicability::AnyProduct,
    }
}

pub fn percentage_off(
    id: &str,
    priority: i32,
    stackability: Stackability,
    product_ref: Option<&str>,
    bps: u32,
) -> Promotion {
    promotion(
        id,
        priority,
        stackability,
        PromotionRule::PercentageOff {
            applies_to: applies_to(product_ref),
            percentage: Percentage::from_bps(bps).unwrap(),
        },
    )
}

pub fn fixed_off(
    id: &str,
    priority: i32,
    stackability: Stackability,
    product_ref: Option<&str>,
    minor: i64,
) -> Promotion {
    promotion(
        id,
        priority,
        stackability,
        PromotionRule::FixedAmountOff {
            applies_to: applies_to(product_ref),
            amount: Money::from_minor(minor, Currency::Mxn),
        },
    )
}

pub fn buy_n_get_m(
    id: &str,
    priority: i32,
    stackability: Stackability,
    product_ref: Option<&str>,
    buy: i64,
    get: i64,
) -> Promotion {
    promotion(
        id,
        priority,
        stackability,
        PromotionRule::BuyNGetM {
            applies_to: applies_to(product_ref),
            buy: Quantity::from_units(buy).unwrap(),
            get: Quantity::from_units(get).unwrap(),
        },
    )
}

pub fn threshold(id: &str, priority: i32, minimum_minor: i64, amount_minor: i64) -> Promotion {
    promotion(
        id,
        priority,
        Stackability::Stackable,
        PromotionRule::BasketThreshold {
            minimum: Money::from_minor(minimum_minor, Currency::Mxn),
            amount: Money::from_minor(amount_minor, Currency::Mxn),
        },
    )
}

/// `net + tax == gross` on every line, and the totals are the sum of the
/// lines. The invariant promotions must never break.
pub fn assert_tax_identity(transaction: &RetailTransaction) {
    let state = transaction.state();
    let mut net = 0;
    let mut tax = 0;
    let mut gross = 0;
    let mut extended = 0;
    let mut discount = 0;
    for line in &state.lines {
        assert_eq!(
            line.taxable_amount.minor() + line.tax_amount.minor(),
            line.line_total.minor(),
            "line {} breaks net + tax == gross",
            line.line_number
        );
        assert!(
            line.line_total.minor() >= 0,
            "line {} went negative",
            line.line_number
        );
        net += line.taxable_amount.minor();
        tax += line.tax_amount.minor();
        gross += line.line_total.minor();
        extended += line.extended_amount.minor();
        discount += line.discount_amount.minor();
    }
    let totals = &state.totals;
    assert_eq!(totals.net_total.minor(), net);
    assert_eq!(totals.tax_total.minor(), tax);
    assert_eq!(totals.grand_total.minor(), gross);
    assert_eq!(totals.subtotal.minor(), extended);
    assert_eq!(totals.discount_total.minor(), discount);
    // The discounts the engine reported and the ones the lines carry are the
    // same money.
    let reported: i64 = transaction
        .applied_promotions()
        .iter()
        .map(|applied| applied.amount.minor())
        .sum();
    assert_eq!(reported, discount, "adjustments must sum to the discounts");
}

/// Cash tender through the payment engine, answering with the sale.
pub fn pay_cash(core: &GoldenCore, amount_minor: i64) -> RetailTransaction {
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Cash,
        amount_minor,
    })
    .unwrap()
    .transaction
}

pub type ScriptedGoldenCore =
    EdgeApplication<SqliteEdgeStore, FixedClock, SequentialIdFactory, SimulatorPaymentProvider>;

/// Golden core whose card provider follows a script.
pub fn scripted_golden_core(script: SimulatorScript) -> ScriptedGoldenCore {
    let instant: DateTime<Utc> = DateTime::parse_from_rfc3339(FROZEN_AT)
        .unwrap()
        .with_timezone(&Utc);
    EdgeApplication::with_hardware(
        SqliteEdgeStore::open_in_memory().unwrap(),
        FixedClock::frozen(instant),
        SequentialIdFactory::new(),
        SimulatorPaymentProvider::new(script),
        Hardware::simulated(),
    )
}

/// Import and activate a catalog on a scripted core, then open the register.
pub fn open_scripted_register(core: &ScriptedGoldenCore, catalog: &CatalogFixture) {
    core.init_device(InitDeviceCommand {
        tenant_id: TenantId::new("TENANT-DEMO").unwrap(),
        store_id: StoreId::new("STORE-DEMO").unwrap(),
        currency: Currency::Mxn,
        allow_negative_inventory: true,
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

/// Scan on a scripted core.
pub fn scripted_scan(core: &ScriptedGoldenCore, barcode: &str, quantity_thousandths: i64) {
    core.add_item(AddItemCommand {
        request_id: None,
        selector: ProductSelector::Barcode(Barcode::new(barcode).unwrap()),
        quantity: Quantity::from_thousandths(quantity_thousandths).unwrap(),
        selections: Vec::new(),
        preparation_note: None,
    })
    .unwrap();
}

/// Open the register with an explicit float, for the cash goldens.
pub fn open_register_with_float(core: &GoldenCore, opening_cash_minor: i64) {
    core.init_device(InitDeviceCommand {
        tenant_id: TenantId::new("TENANT-DEMO").unwrap(),
        store_id: StoreId::new("STORE-DEMO").unwrap(),
        currency: Currency::Mxn,
        allow_negative_inventory: true,
    })
    .unwrap();
    golden_catalog(1).activate(core).unwrap();
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

/// Import an opening position: `(product_ref, thousandths)` pairs.
pub fn golden_bootstrap(core: &GoldenCore, items: &[(&str, i64)]) -> InventoryBootstrapState {
    core.import_inventory_bootstrap(ImportInventoryBootstrapCommand {
        request_id: None,
        source_snapshot_id: "WMS-SNAPSHOT-1".to_owned(),
        generated_at: Timestamp::parse_rfc3339("2026-10-08T14:00:00Z").unwrap(),
        items: items
            .iter()
            .map(|(product_ref, thousandths)| BootstrapItem {
                product_ref: ProductRef::new(*product_ref).unwrap(),
                quantity: Quantity::from_thousandths(*thousandths).unwrap(),
            })
            .collect(),
    })
    .unwrap()
}

pub fn golden_position(core: &GoldenCore, product_ref: &str) -> InventoryPositionView {
    core.get_inventory_position(ProductRef::new(product_ref).unwrap(), 50)
        .unwrap()
}

// ----------------------------------------------------------------- refunds

/// The authority a refund needs, as the composition root would resolve it.
pub fn central_authority(transaction_id: TransactionId) -> RefundAuthorizationContext {
    RefundAuthorizationContext {
        original_transaction_id: transaction_id,
        authority: RefundAuthority::CentralAccepted,
        authorization_ref: Some("GOLDEN-CENTRAL".to_owned()),
    }
}

/// `(original_line_number, thousandths)` pairs as a refund request.
pub fn refund_lines(lines: &[(i64, i64)]) -> Vec<RefundLineRequest> {
    lines
        .iter()
        .map(|(line_number, thousandths)| RefundLineRequest {
            original_line_number: *line_number,
            quantity: Quantity::from_thousandths(*thousandths).unwrap(),
        })
        .collect()
}

/// A whole refund on a golden core: open it, take the goods, pay it back.
pub fn golden_refund(
    core: &GoldenCore,
    transaction_id: TransactionId,
    lines: Vec<RefundLineRequest>,
    fiscal_action: RefundFiscalStatus,
) -> RefundView {
    let refund = core
        .create_refund(CreateRefundCommand {
            request_id: None,
            authorization: central_authority(transaction_id),
            lines,
            reason_code: "CUSTOMER_RETURN".to_owned(),
            fiscal_action,
        })
        .unwrap();
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

/// A finished cash sale on a golden core, answering with its id.
pub fn golden_finalized_sale(
    core: &GoldenCore,
    barcode: &str,
    quantity_thousandths: i64,
    tendered_minor: i64,
) -> TransactionId {
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    scan(core, barcode, quantity_thousandths);
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(core, tendered_minor);
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap()
        .id()
}

/// A finished cash sale of the 16% product: 116.00, tendered `tendered_minor`.
pub fn golden_cash_sale(core: &GoldenCore, tendered_minor: i64) {
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    scan(core, BARCODE_16, 1_000);
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

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod line_ending_tests {
    use super::normalize_line_endings;

    /// The same snapshot checked out two ways is the same snapshot.
    #[test]
    fn a_crlf_checkout_matches_an_lf_golden() {
        let lf = "{\n  \"a\": 1\n}\n";
        let crlf = "{\r\n  \"a\": 1\r\n}\r\n";
        assert_eq!(normalize_line_endings(lf), normalize_line_endings(crlf));
        // Old Mac line endings too, for the same reason.
        assert_eq!(
            normalize_line_endings(lf),
            normalize_line_endings("{\r  \"a\": 1\r}\r")
        );
    }

    /// And nothing else is forgiven.
    #[test]
    fn only_the_line_ending_is_normalised() {
        let lf = "{\n  \"a\": 1\n}\n";
        for different in [
            "{\n  \"a\": 2\n}\n",     // a different value
            "{\n  \"b\": 1\n}\n",     // a different field
            "{\n   \"a\": 1\n}\n",    // different indentation
            "{\n  \"a\":1\n}\n",      // different spacing
            "{\n  \"a\": 1\n}",       // a missing trailing newline
            "{\n  \"a\": \"1\"\n}\n", // a number that became a string
        ] {
            assert_ne!(
                normalize_line_endings(lf),
                normalize_line_endings(different),
                "`{different}` must not match"
            );
        }
    }

    /// Borrowed when there is nothing to do, so the common path copies
    /// nothing.
    #[test]
    fn lf_input_is_not_copied() {
        assert!(matches!(
            normalize_line_endings("already\nlf\n"),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}
