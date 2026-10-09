//! Deterministic protocol output for known sales.
//!
//! Prices and tax rates come from an imported catalog snapshot, exactly as in
//! production: nothing in these tests states a price. Time is frozen and ids
//! are sequential, so the output is byte-stable.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_golden_tests::*;
use edge_protocol::{CatalogStatusV1, EdgeStateV1, RetailTransactionV1};

/// A single line at 16% tax, paid with change: the canonical smoke case.
#[test]
fn golden_single_line_cash_sale() {
    let core = golden_core();
    open_register_with(&core, &golden_catalog(1));
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    scan(&core, BARCODE_16, 1_000);
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(&core, 20_000);
    let finalized = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();

    // The core resolved everything from the catalog.
    let line = &finalized.state().lines[0];
    assert_eq!(line.description, "Gravado 16%");
    assert_eq!(line.unit_price.minor(), 10_000);
    assert_eq!(line.tax_amount.minor(), 1_600);
    assert_eq!(line.line_total.minor(), 11_600);
    assert_eq!(line.barcode.as_deref(), Some(BARCODE_16));

    let dto = RetailTransactionV1::from_domain(&finalized).unwrap();
    assert_golden(
        "single_line_cash_sale",
        &serde_json::to_value(&dto).unwrap(),
    );
}

/// Mixed rates and a fractional quantity: exercises every rounding path of the
/// tax-exclusive math.
///
/// Line discounts are deliberately absent: no client may state one in this
/// milestone, so the golden case reflects what the API can actually express.
/// The discount arithmetic stays covered by the domain unit tests.
#[test]
fn golden_mixed_rates_and_rounding() {
    let core = golden_core();
    open_register_with(&core, &golden_catalog(1));
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    // 333 x 2.5 = 832.5 -> 833 ; tax 16% of 833 = 133.28 -> 133
    scan(&core, BARCODE_FRACTIONAL, 2_500);
    // 1999 x 3 = 5997 ; tax 0
    scan(&core, BARCODE_0, 3_000);
    // 10000 x 1 = 10000 ; tax 8% = 800
    scan(&core, BARCODE_8, 1_000);
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let totals = &totalized.state().totals;
    assert_eq!(totals.subtotal.minor(), 833 + 5_997 + 10_000);
    assert_eq!(totals.net_total.minor(), 833 + 5_997 + 10_000);
    assert_eq!(totals.tax_total.minor(), 133 + 800);
    assert_eq!(totals.grand_total.minor(), 17_763);
    assert_eq!(totals.discount_total.minor(), 0);

    let dto = RetailTransactionV1::from_domain(&totalized).unwrap();
    assert_golden(
        "mixed_rates_and_rounding",
        &serde_json::to_value(&dto).unwrap(),
    );
}

/// Tax-inclusive shelf prices: the customer pays the quoted amount and the
/// core reports how much of it is tax.
#[test]
fn golden_tax_inclusive_pricing() {
    let core = golden_core();
    open_register_with(&core, &golden_catalog(1));
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    scan(&core, BARCODE_INCLUSIVE_16, 1_000);
    scan(&core, BARCODE_INCLUSIVE_8, 1_000);
    scan(&core, BARCODE_INCLUSIVE_0, 1_000);
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let lines = &totalized.state().lines;
    // 116.00 including 16% -> net 100.00, tax 16.00
    assert_eq!(lines[0].taxable_amount.minor(), 10_000);
    assert_eq!(lines[0].tax_amount.minor(), 1_600);
    assert_eq!(lines[0].line_total.minor(), 11_600);
    // 108.00 including 8% -> net 100.00, tax 8.00
    assert_eq!(lines[1].taxable_amount.minor(), 10_000);
    assert_eq!(lines[1].tax_amount.minor(), 800);
    assert_eq!(lines[1].line_total.minor(), 10_800);
    // 0% inclusive: net equals gross, tax is zero
    assert_eq!(lines[2].taxable_amount.minor(), 9_999);
    assert_eq!(lines[2].tax_amount.minor(), 0);
    assert_eq!(lines[2].line_total.minor(), 9_999);

    let totals = &totalized.state().totals;
    assert_eq!(totals.net_total.minor(), 10_000 + 10_000 + 9_999);
    assert_eq!(totals.tax_total.minor(), 1_600 + 800);
    assert_eq!(totals.grand_total.minor(), 11_600 + 10_800 + 9_999);
    // The quoted total and the grand total agree: inclusive prices are final.
    assert_eq!(totals.subtotal.minor(), totals.grand_total.minor());

    let dto = RetailTransactionV1::from_domain(&totalized).unwrap();
    assert_golden(
        "tax_inclusive_pricing",
        &serde_json::to_value(&dto).unwrap(),
    );
}

/// Amounts where backing the tax out cannot divide evenly. Freezes the
/// rounding policy and proves `net + tax == gross` line by line.
#[test]
fn golden_inclusive_rounding_boundaries() {
    let core = golden_core();
    open_register_with(&core, &golden_catalog(1));
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    // 0.99 x 3 = 2.97 inclusive at 16% -> net 2.56, tax 0.41
    scan(&core, BARCODE_ODD_INCLUSIVE, 3_000);
    // 0.99 x 1 = 0.99 inclusive at 16% -> net 0.85, tax 0.14
    scan(&core, BARCODE_ODD_INCLUSIVE, 1_000);
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    for line in &totalized.state().lines {
        assert_eq!(
            line.taxable_amount
                .checked_add(line.tax_amount)
                .unwrap()
                .minor(),
            line.line_total.minor(),
            "net + tax must equal the line total"
        );
        assert_eq!(line.line_total.minor(), line.extended_amount.minor());
    }
    let lines = &totalized.state().lines;
    assert_eq!(lines[0].extended_amount.minor(), 297);
    assert_eq!(lines[0].taxable_amount.minor(), 256);
    assert_eq!(lines[0].tax_amount.minor(), 41);
    assert_eq!(lines[1].extended_amount.minor(), 99);
    assert_eq!(lines[1].taxable_amount.minor(), 85);
    assert_eq!(lines[1].tax_amount.minor(), 14);

    let dto = RetailTransactionV1::from_domain(&totalized).unwrap();
    assert_golden(
        "inclusive_rounding_boundaries",
        &serde_json::to_value(&dto).unwrap(),
    );
}

/// The diagnostic payload the Tauri screen renders.
#[test]
fn golden_edge_state_mid_sale() {
    let core = golden_core();
    open_register_with(&core, &golden_catalog(1));
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    scan(&core, BARCODE_16, 1_000);
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(&core, 5_000);

    let state = core.get_edge_state().unwrap();
    let dto = EdgeStateV1::from_domain(&state).unwrap();
    assert_golden("edge_state_mid_sale", &serde_json::to_value(&dto).unwrap());
}

/// A store's own price book overrides the tenant-wide base, structurally.
#[test]
fn golden_store_price_override() {
    let core = golden_core();
    open_register_with(&core, &golden_catalog(1));
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    scan(&core, BARCODE_STORE_OVERRIDE, 1_000);
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let line = &totalized.state().lines[0];
    // 89.00 from the store book, not 100.00 from the base book.
    assert_eq!(line.unit_price.minor(), 8_900);
    assert_eq!(
        line.price_book_id.as_ref().map(ToString::to_string),
        Some(STORE_BOOK.to_owned())
    );
    assert_eq!(line.tax_amount.minor(), 1_424);

    let dto = RetailTransactionV1::from_domain(&totalized).unwrap();
    assert_golden("store_price_override", &serde_json::to_value(&dto).unwrap());
}

/// A scheduled price is not in force before its `valid_from`, and is after it.
#[test]
fn golden_scheduled_price_change() {
    // Before the change: the outgoing price applies.
    let before = golden_core();
    open_register_with(&before, &golden_catalog(1));
    before
        .start_transaction(StartTransactionCommand::default())
        .unwrap();
    scan(&before, BARCODE_SCHEDULED, 1_000);
    let totalized = before
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(totalized.state().lines[0].unit_price.minor(), 10_000);
    let dto = RetailTransactionV1::from_domain(&totalized).unwrap();
    assert_golden(
        "scheduled_price_before",
        &serde_json::to_value(&dto).unwrap(),
    );

    // After it: the incoming price applies, from the same snapshot.
    let after = golden_core_at(SCHEDULED_FROM);
    open_register_with(&after, &golden_catalog(1));
    after
        .start_transaction(StartTransactionCommand::default())
        .unwrap();
    scan(&after, BARCODE_SCHEDULED, 1_000);
    let totalized = after
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(totalized.state().lines[0].unit_price.minor(), 12_000);
    let dto = RetailTransactionV1::from_domain(&totalized).unwrap();
    assert_golden(
        "scheduled_price_after",
        &serde_json::to_value(&dto).unwrap(),
    );
}

/// The catalog the register is running, including the hash the core computed
/// from its own rows.
#[test]
fn golden_catalog_status() {
    let core = golden_core();
    open_register_with(&core, &golden_catalog(1));
    let status = core.get_catalog_status().unwrap();
    let active = status.active.as_ref().expect("a catalog must be active");
    assert_eq!(active.stored.products, 10);
    assert_eq!(active.stored.barcodes, 10);
    assert_eq!(active.stored.tax_profiles, 3);
    assert_eq!(active.stored.price_books, 2);
    // Eight products with one price, one with a store override, one scheduled.
    assert_eq!(active.stored.price_entries, 12);
    // The core stores the hash it computed, not the one it was given.
    assert_eq!(
        active.snapshot.state().content_hash.as_deref(),
        Some(active.snapshot.state().declared_content_hash.as_str())
    );

    let dto = CatalogStatusV1::from(&status);
    assert_golden("catalog_status", &serde_json::to_value(&dto).unwrap());
}
