//! Deterministic protocol output for promoted sales.
//!
//! Every promotion here comes from the imported snapshot: nothing in these
//! tests invents a discount, and no client states one. The point of each case
//! is that the money is byte-stable *and* that `net + tax == gross` still
//! holds on every line.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_golden_tests::*;
use edge_protocol::RetailTransactionV1;

/// 10% off one line, then a 15.00 fixed discount on the same line: the
/// stacking order is priority DESC, so the percentage lands first.
#[test]
fn golden_percentage_promotion() {
    let core = golden_core();
    open_register_with(&core, &promotional_catalog(1));
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    // 100.00: 10% off leaves 90.00, then 15.00 off leaves 75.00.
    scan(&core, BARCODE_16, 1_000);
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let line = &totalized.state().lines[0];
    assert_eq!(line.extended_amount.minor(), 10_000);
    assert_eq!(line.discount_amount.minor(), 2_500);
    assert_eq!(line.taxable_amount.minor(), 7_500);
    assert_eq!(line.tax_amount.minor(), 1_200);
    assert_eq!(line.line_total.minor(), 8_700);
    assert_tax_identity(&totalized);

    // Two adjustments, highest priority first, each one auditable on its own.
    let applied = totalized.applied_promotions();
    assert_eq!(applied.len(), 2);
    assert_eq!(applied[0].code, "PROMO-PCT-16");
    assert_eq!(applied[0].amount.minor(), 1_000);
    assert_eq!(applied[0].adjustment_id, "PROMO-PCT-16#line:1");
    assert_eq!(applied[1].code, "PROMO-FIX-16");
    assert_eq!(applied[1].amount.minor(), 1_500);

    let dto = RetailTransactionV1::from_domain(&totalized).unwrap();
    assert_golden("promotion_percentage", &serde_json::to_value(&dto).unwrap());
}

/// The fixed-amount rule alone, on a quantity that makes the percentage rule
/// inapplicable: a different product, so only one promotion can match.
#[test]
fn golden_fixed_amount_promotion() {
    let core = golden_core();
    let catalog = golden_catalog(1).promoting(fixed_off(
        "PROMO-FIX-ONLY",
        0,
        edge_catalog::promotions::Stackability::Stackable,
        Some("SKU-0"),
        500,
    ));
    open_register_with(&core, &catalog);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    // 19.99 x 3 = 59.97, minus 5.00 = 54.97 at 0% tax.
    scan(&core, BARCODE_0, 3_000);
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let line = &totalized.state().lines[0];
    assert_eq!(line.extended_amount.minor(), 5_997);
    assert_eq!(line.discount_amount.minor(), 500);
    assert_eq!(line.line_total.minor(), 5_497);
    assert_eq!(line.tax_amount.minor(), 0);
    assert_tax_identity(&totalized);

    let dto = RetailTransactionV1::from_domain(&totalized).unwrap();
    assert_golden("promotion_fixed", &serde_json::to_value(&dto).unwrap());
}

/// Buy 2 get 1, at a quantity of 6: it must apply **twice**.
#[test]
fn golden_buy_n_get_m_promotion() {
    let core = golden_core();
    open_register_with(&core, &promotional_catalog(1));
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    // 6 x 100.00 at 8%: two free units, so 400.00 is taxable.
    scan(&core, BARCODE_8, 6_000);
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let line = &totalized.state().lines[0];
    assert_eq!(line.extended_amount.minor(), 60_000);
    assert_eq!(line.discount_amount.minor(), 20_000);
    assert_eq!(line.taxable_amount.minor(), 40_000);
    assert_eq!(line.tax_amount.minor(), 3_200);
    assert_eq!(line.line_total.minor(), 43_200);
    assert_tax_identity(&totalized);

    let applied = totalized.applied_promotions();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].reason, "BUY_N_GET_M");
    assert_eq!(
        applied[0].detail.as_deref(),
        Some("2 application(s), 2 free unit(s)")
    );

    let dto = RetailTransactionV1::from_domain(&totalized).unwrap();
    assert_golden(
        "promotion_buy_n_get_m",
        &serde_json::to_value(&dto).unwrap(),
    );
}

/// A line promotion plus a basket threshold, allocated back onto the lines so
/// each one still computes its own tax.
#[test]
fn golden_stacked_promotions_with_basket_threshold() {
    let core = golden_core();
    open_register_with(&core, &promotional_catalog(1));
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    // 100.00 at 16%, discounted to 75.00 by the two line rules.
    scan(&core, BARCODE_16, 1_000);
    // 6 x 100.00 at 8%, minus two free units: 400.00.
    scan(&core, BARCODE_8, 6_000);
    // 19.99 x 2 = 39.98 at 0%.
    scan(&core, BARCODE_0, 2_000);
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    // The threshold is measured on what is left after the line rules:
    // 75.00 + 400.00 + 39.98 = 514.98, which clears 500.00.
    let applied = totalized.applied_promotions();
    let threshold = applied
        .iter()
        .find(|applied| applied.code == "PROMO-THRESHOLD")
        .expect("the basket threshold applies");
    assert_eq!(threshold.amount.minor(), 5_000);
    assert_eq!(threshold.target.kind(), "BASKET");
    assert_eq!(threshold.adjustment_id, "PROMO-THRESHOLD#basket");

    // The 50.00 is distributed over the three lines and sums back exactly.
    let allocated: i64 = totalized
        .state()
        .lines
        .iter()
        .map(|line| line.discount_amount.minor())
        .sum();
    assert_eq!(allocated, 2_500 + 20_000 + 5_000);
    assert_tax_identity(&totalized);

    let dto = RetailTransactionV1::from_domain(&totalized).unwrap();
    assert_golden("promotion_stacked", &serde_json::to_value(&dto).unwrap());
}

/// An exclusive promotion applies alone: the basket threshold cannot touch the
/// line it owns, and the allocation skips it.
#[test]
fn golden_exclusive_promotion() {
    let core = golden_core();
    open_register_with(&core, &promotional_catalog(1));
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    // 7 x 89.00 (store book) = 623.00, 30% off = 186.90 discount.
    scan(&core, BARCODE_STORE_OVERRIDE, 7_000);
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let applied = totalized.applied_promotions();
    // The exclusive line rule, and the basket threshold on what remains.
    assert_eq!(applied[0].code, "PROMO-EXC-STORE");
    assert_eq!(applied[0].stackability, "EXCLUSIVE");
    assert_eq!(applied[0].amount.minor(), 18_690);
    // No other *line* promotion may join it.
    assert_eq!(
        applied
            .iter()
            .filter(|applied| applied.target.kind() == "LINE")
            .count(),
        1
    );
    assert_tax_identity(&totalized);

    let dto = RetailTransactionV1::from_domain(&totalized).unwrap();
    assert_golden("promotion_exclusive", &serde_json::to_value(&dto).unwrap());
}

/// A promotion on a tax-inclusive price: the discount comes off the gross, and
/// the tax is re-derived from the discounted gross.
#[test]
fn golden_tax_inclusive_promotion() {
    let core = golden_core();
    open_register_with(&core, &promotional_catalog(1));
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    // 116.00 inclusive of 16%, 20% off = 23.20 discount, 92.80 gross.
    scan(&core, BARCODE_INCLUSIVE_16, 1_000);
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let line = &totalized.state().lines[0];
    assert_eq!(line.extended_amount.minor(), 11_600);
    assert_eq!(line.discount_amount.minor(), 2_320);
    assert_eq!(line.line_total.minor(), 9_280);
    // 9280 / 1.16 = 8000 exactly, so the tax is 1280.
    assert_eq!(line.taxable_amount.minor(), 8_000);
    assert_eq!(line.tax_amount.minor(), 1_280);
    assert_tax_identity(&totalized);

    let dto = RetailTransactionV1::from_domain(&totalized).unwrap();
    assert_golden(
        "promotion_tax_inclusive",
        &serde_json::to_value(&dto).unwrap(),
    );
}
