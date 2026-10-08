//! Deterministic protocol output for known sales.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_golden_tests::*;
use edge_protocol::{EdgeStateV1, RetailTransactionV1};

/// A single line at 16% tax: the canonical smoke case.
#[test]
fn golden_single_line_cash_sale() {
    let core = golden_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    add_item(
        &core,
        ItemSpec {
            product: "SKU-001",
            description: "Producto prueba",
            price_minor: 10_000,
            tax_rate_bps: 1_600,
            quantity_thousandths: 1_000,
            discount_minor: 0,
        },
    );
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.commit_cash_payment(CommitCashPaymentCommand {
        request_id: None,
        amount_minor: 20_000,
    })
    .unwrap();
    let finalized = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();

    let dto = RetailTransactionV1::from_domain(&finalized).unwrap();
    assert_golden(
        "single_line_cash_sale",
        &serde_json::to_value(&dto).unwrap(),
    );
}

/// Mixed tax rates, a fractional quantity and a line discount: exercises every
/// rounding path of the pricing math.
#[test]
fn golden_mixed_rates_and_rounding() {
    let core = golden_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    add_item(
        &core,
        ItemSpec {
            product: "SKU-001",
            description: "Taxed at 16%",
            price_minor: 333,
            tax_rate_bps: 1_600,
            quantity_thousandths: 2_500,
            discount_minor: 0,
        },
    );
    add_item(
        &core,
        ItemSpec {
            product: "SKU-002",
            description: "Zero rated",
            price_minor: 1_999,
            tax_rate_bps: 0,
            quantity_thousandths: 3_000,
            discount_minor: 0,
        },
    );
    add_item(
        &core,
        ItemSpec {
            product: "SKU-003",
            description: "Discounted 8%",
            price_minor: 12_345,
            tax_rate_bps: 800,
            quantity_thousandths: 1_000,
            discount_minor: 1_111,
        },
    );
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    // Guard the arithmetic explicitly, not only through the golden file.
    // Line 1: 333 * 2.5 = 832.5 -> 833 ; tax 16% of 833 = 133.28 -> 133
    // Line 2: 1999 * 3 = 5997 ; tax 0
    // Line 3: 12345 - 1111 = 11234 ; tax 8% of 11234 = 898.72 -> 899
    assert_eq!(
        totalized.state().totals.subtotal.minor(),
        833 + 5_997 + 12_345
    );
    assert_eq!(totalized.state().totals.discount_total.minor(), 1_111);
    assert_eq!(totalized.state().totals.tax_total.minor(), 133 + 899);
    assert_eq!(totalized.state().totals.grand_total.minor(), 19_096);

    let dto = RetailTransactionV1::from_domain(&totalized).unwrap();
    assert_golden(
        "mixed_rates_and_rounding",
        &serde_json::to_value(&dto).unwrap(),
    );
}

/// The diagnostic payload the Tauri screen will render.
#[test]
fn golden_edge_state_mid_sale() {
    let core = golden_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    add_item(
        &core,
        ItemSpec {
            product: "SKU-001",
            description: "Producto prueba",
            price_minor: 10_000,
            tax_rate_bps: 1_600,
            quantity_thousandths: 1_000,
            discount_minor: 0,
        },
    );
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.commit_cash_payment(CommitCashPaymentCommand {
        request_id: None,
        amount_minor: 5_000,
    })
    .unwrap();

    let state = core.get_edge_state().unwrap();
    let dto = EdgeStateV1::from_domain(&state).unwrap();
    assert_golden("edge_state_mid_sale", &serde_json::to_value(&dto).unwrap());
}
