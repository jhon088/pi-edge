//! The promotion engine against real snapshots in SQLite.
//!
//! The ranking and arithmetic rules are pinned by the unit tests in
//! `edge-catalog`; these check what only the whole runtime can show: that a
//! basket is re-evaluated on every change, that what applied is materialised
//! on the sale, that a sale keeps the promotions of the snapshot it captured,
//! and that the journal says so exactly once.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::testing::{CatalogFixture, FixturePrice, FixtureProduct};
use edge_catalog::promotions::{
    NewPromotion, ProductApplicability, Promotion, PromotionRule, Stackability,
};
use edge_catalog::scope::CatalogScope;
use edge_catalog::CatalogError;
use edge_domain::prelude::*;
use edge_integration_tests::*;

const BARCODE_A: &str = "7500000001001";
const BARCODE_B: &str = "7500000001002";
const BARCODE_INCLUSIVE: &str = "7500000001003";

fn product_a(price_minor: i64) -> FixtureProduct {
    FixtureProduct::new("SKU-A", "Producto A", price_minor)
        .sku("SKU-A")
        .barcode(BARCODE_A)
}

fn product_b(price_minor: i64) -> FixtureProduct {
    FixtureProduct::new("SKU-B", "Producto B", price_minor)
        .sku("SKU-B")
        .barcode(BARCODE_B)
}

/// 116.00 including 16% tax.
fn inclusive_product() -> FixtureProduct {
    FixtureProduct::new("SKU-INC", "Producto con IVA incluido", 0)
        .sku("SKU-INC")
        .barcode(BARCODE_INCLUSIVE)
        .priced(FixturePrice::base(11_600).inclusive())
}

fn promotion(
    id: &str,
    priority: i32,
    stackability: Stackability,
    scope: CatalogScope,
    rule: PromotionRule,
) -> Promotion {
    Promotion::import(NewPromotion {
        promotion_id: PromotionId::new(id).unwrap(),
        code: id.to_owned(),
        name: format!("Promoción {id}"),
        active: true,
        priority,
        stackability,
        scope,
        valid_from: None,
        valid_until: None,
        rule,
    })
    .unwrap()
}

fn applies_to(product_ref: &str) -> ProductApplicability {
    ProductApplicability::Product {
        product_ref: ProductRef::new(product_ref).unwrap(),
    }
}

fn percentage_off(id: &str, priority: i32, product_ref: &str, bps: u32) -> Promotion {
    promotion(
        id,
        priority,
        Stackability::Stackable,
        CatalogScope::Tenant,
        PromotionRule::PercentageOff {
            applies_to: applies_to(product_ref),
            percentage: Percentage::from_bps(bps).unwrap(),
        },
    )
}

fn buy_two_get_one(id: &str, product_ref: &str) -> Promotion {
    promotion(
        id,
        0,
        Stackability::Stackable,
        CatalogScope::Tenant,
        PromotionRule::BuyNGetM {
            applies_to: applies_to(product_ref),
            buy: Quantity::from_units(2).unwrap(),
            get: Quantity::from_units(1).unwrap(),
        },
    )
}

fn basket_threshold(id: &str, minimum_minor: i64, amount_minor: i64) -> Promotion {
    promotion(
        id,
        0,
        Stackability::Stackable,
        CatalogScope::Tenant,
        PromotionRule::BasketThreshold {
            minimum: Money::from_minor(minimum_minor, Currency::Mxn),
            amount: Money::from_minor(amount_minor, Currency::Mxn),
        },
    )
}

fn register_with(core: &TestCore, catalog: CatalogFixture) {
    init_device(core);
    catalog.activate(core).unwrap();
    open_day_and_session(core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
}

/// Sum of the line discounts, which is the only place a discount can live.
fn total_discount(transaction: &RetailTransaction) -> i64 {
    transaction
        .state()
        .lines
        .iter()
        .map(|line| line.discount_amount.minor())
        .sum()
}

fn assert_tax_identity(transaction: &RetailTransaction) {
    for line in &transaction.state().lines {
        assert_eq!(
            line.taxable_amount.minor() + line.tax_amount.minor(),
            line.line_total.minor(),
            "line {} breaks net + tax == gross",
            line.line_number
        );
        assert!(line.line_total.minor() >= 0);
        assert!(line.discount_amount.minor() <= line.extended_amount.minor());
    }
}

// ------------------------------------------------------------ the happy path

#[test]
fn a_promotion_in_the_snapshot_discounts_the_line_it_applies_to() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(10_000))
            .promoting(percentage_off("PCT", 0, "SKU-A", 1_000)),
    );

    let transaction = core.add_item(scan(BARCODE_A, 1)).unwrap();
    let line = &transaction.state().lines[0];
    assert_eq!(line.discount_amount.minor(), 1_000);
    assert_eq!(line.taxable_amount.minor(), 9_000);
    assert_eq!(line.tax_amount.minor(), 1_440);
    assert_eq!(line.line_total.minor(), 10_440);
    assert_tax_identity(&transaction);

    // And the adjustment is on the sale, not only in the arithmetic.
    let applied = transaction.applied_promotions();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].promotion_id, "PCT");
    assert_eq!(applied[0].reason, "PERCENTAGE_OFF");
    assert_eq!(applied[0].target, PromotionTarget::Line { line_number: 1 });
}

#[test]
fn a_basket_with_no_applicable_promotion_is_priced_exactly_as_before() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(10_000))
            // Only applies to a product this basket does not carry.
            .with(product_b(5_000))
            .promoting(percentage_off("PCT-B", 0, "SKU-B", 5_000)),
    );

    let transaction = core.add_item(scan(BARCODE_A, 1)).unwrap();
    assert_eq!(total_discount(&transaction), 0);
    assert!(transaction.applied_promotions().is_empty());
    let line = &transaction.state().lines[0];
    assert_eq!(line.taxable_amount.minor(), 10_000);
    assert_eq!(line.tax_amount.minor(), 1_600);
    assert_eq!(line.line_total.minor(), 11_600);
}

#[test]
fn the_totals_account_for_promotions_as_discounts() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(10_000))
            .promoting(percentage_off("PCT", 0, "SKU-A", 1_000)),
    );
    core.add_item(scan(BARCODE_A, 2)).unwrap();
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let totals = &totalized.state().totals;
    assert_eq!(totals.subtotal.minor(), 20_000);
    assert_eq!(totals.discount_total.minor(), 2_000);
    assert_eq!(totals.net_total.minor(), 18_000);
    assert_eq!(totals.tax_total.minor(), 2_880);
    assert_eq!(totals.grand_total.minor(), 20_880);
}

// ------------------------------------------------- re-evaluation on change

#[test]
fn adding_a_second_line_re_evaluates_the_whole_basket() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(30_000))
            .with(product_b(30_000))
            .promoting(basket_threshold("THR", 50_000, 5_000)),
    );

    // One line is below the threshold: nothing applies.
    let first = core.add_item(scan(BARCODE_A, 1)).unwrap();
    assert_eq!(total_discount(&first), 0);

    // The second line clears it, and the discount lands on *both* lines.
    let second = core.add_item(scan(BARCODE_B, 1)).unwrap();
    assert_eq!(total_discount(&second), 5_000);
    assert_eq!(second.state().lines[0].discount_amount.minor(), 2_500);
    assert_eq!(second.state().lines[1].discount_amount.minor(), 2_500);
    assert_tax_identity(&second);
}

#[test]
fn removing_a_line_retracts_the_promotion_it_had_earned() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(30_000))
            .with(product_b(30_000))
            .promoting(basket_threshold("THR", 50_000, 5_000)),
    );
    core.add_item(scan(BARCODE_A, 1)).unwrap();
    let both = core.add_item(scan(BARCODE_B, 1)).unwrap();
    assert_eq!(both.applied_promotions().len(), 1);

    let after = core
        .remove_item(RemoveItemCommand {
            request_id: None,
            line_number: 2,
        })
        .unwrap();
    assert_eq!(after.state().lines.len(), 1);
    assert_eq!(total_discount(&after), 0);
    assert!(after.applied_promotions().is_empty());

    // The retracted adjustment is gone from storage too, not just from memory.
    let reloaded = core.get_active_transaction().unwrap().unwrap();
    assert!(reloaded.applied_promotions().is_empty());
    assert_eq!(reloaded.state().lines.len(), 1);
}

#[test]
fn changing_a_quantity_re_evaluates_buy_n_get_m() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(1_000))
            .promoting(buy_two_get_one("BOGO", "SKU-A")),
    );

    let three = core.add_item(scan(BARCODE_A, 3)).unwrap();
    assert_eq!(total_discount(&three), 1_000);

    // Six units earn the promotion twice.
    let six = core
        .change_quantity(ChangeQuantityCommand {
            request_id: None,
            line_number: 1,
            quantity: Quantity::from_units(6).unwrap(),
        })
        .unwrap();
    assert_eq!(total_discount(&six), 2_000);
    assert_eq!(
        six.state().lines[0].quantity,
        Quantity::from_units(6).unwrap()
    );
    // The line keeps its identity and its captured price.
    assert_eq!(six.state().lines[0].line_id, three.state().lines[0].line_id);
    assert_eq!(six.state().lines[0].unit_price.minor(), 1_000);

    // Two units earn nothing, and the adjustment disappears.
    let two = core
        .change_quantity(ChangeQuantityCommand {
            request_id: None,
            line_number: 1,
            quantity: Quantity::from_units(2).unwrap(),
        })
        .unwrap();
    assert_eq!(total_discount(&two), 0);
    assert!(two.applied_promotions().is_empty());
}

#[test]
fn a_removed_line_number_is_never_reused() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(1_000))
            .with(product_b(1_000)),
    );
    core.add_item(scan(BARCODE_A, 1)).unwrap();
    core.add_item(scan(BARCODE_B, 1)).unwrap();
    core.remove_item(RemoveItemCommand {
        request_id: None,
        line_number: 1,
    })
    .unwrap();

    let after = core.add_item(scan(BARCODE_A, 1)).unwrap();
    let numbers: Vec<i64> = after
        .state()
        .lines
        .iter()
        .map(|line| line.line_number)
        .collect();
    assert_eq!(numbers, vec![2, 3]);

    // And the counter survives a reload: the next number is still 4.
    let reloaded = core.get_active_transaction().unwrap().unwrap();
    assert_eq!(reloaded.state().next_line_number, 4);
}

#[test]
fn removing_a_line_that_is_not_there_is_refused() {
    let core = in_memory_core();
    register_with(&core, CatalogFixture::new(1).with(product_a(1_000)));
    core.add_item(scan(BARCODE_A, 1)).unwrap();

    let error = core
        .remove_item(RemoveItemCommand {
            request_id: None,
            line_number: 7,
        })
        .unwrap_err();
    assert_eq!(error.kind(), "LINE_NOT_FOUND");
}

#[test]
fn re_evaluating_the_same_basket_changes_nothing() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(10_000))
            .with(product_b(10_000))
            .promoting(percentage_off("PCT", 0, "SKU-A", 1_000)),
    );
    let first = core.add_item(scan(BARCODE_A, 1)).unwrap();
    let adjustments: Vec<String> = first
        .applied_promotions()
        .iter()
        .map(|applied| applied.adjustment_id.clone())
        .collect();
    let discount = total_discount(&first);

    // Adding and removing another line returns the basket to where it was, so
    // the adjustment ids and the money must be identical — the engine keeps no
    // state between calls.
    core.add_item(scan(BARCODE_B, 1)).unwrap();
    let again = core
        .remove_item(RemoveItemCommand {
            request_id: None,
            line_number: 2,
        })
        .unwrap();
    assert_eq!(total_discount(&again), discount);
    assert_eq!(
        again
            .applied_promotions()
            .iter()
            .map(|applied| applied.adjustment_id.clone())
            .collect::<Vec<_>>(),
        adjustments
    );
}

// ------------------------------------------------------------ arbitration

#[test]
fn two_exclusive_promotions_at_one_priority_are_refused_instead_of_guessed() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(10_000))
            .promoting(promotion(
                "EXC-A",
                10,
                Stackability::Exclusive,
                CatalogScope::Tenant,
                PromotionRule::PercentageOff {
                    applies_to: applies_to("SKU-A"),
                    percentage: Percentage::from_bps(1_000).unwrap(),
                },
            ))
            .promoting(promotion(
                "EXC-B",
                10,
                Stackability::Exclusive,
                CatalogScope::Tenant,
                PromotionRule::FixedAmountOff {
                    applies_to: applies_to("SKU-A"),
                    amount: Money::from_minor(2_000, Currency::Mxn),
                },
            )),
    );

    let error = core.add_item(scan(BARCODE_A, 1)).unwrap_err();
    assert_eq!(error.kind(), "AMBIGUOUS_PROMOTION");
    assert!(matches!(
        error,
        edge_application::AppError::Catalog(CatalogError::AmbiguousPromotion { .. })
    ));
    // The refusal is a refusal: nothing was written.
    let transaction = core.get_active_transaction().unwrap().unwrap();
    assert!(transaction.state().lines.is_empty());
}

#[test]
fn a_promotion_scoped_to_another_store_never_applies_here() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(10_000))
            .promoting(promotion(
                "OTHER-STORE",
                0,
                Stackability::Stackable,
                CatalogScope::Store {
                    store_id: StoreId::new("STORE-OTHER").unwrap(),
                },
                PromotionRule::PercentageOff {
                    applies_to: applies_to("SKU-A"),
                    percentage: Percentage::from_bps(5_000).unwrap(),
                },
            )),
    );

    let transaction = core.add_item(scan(BARCODE_A, 1)).unwrap();
    assert_eq!(total_discount(&transaction), 0);
}

#[test]
fn an_inactive_promotion_never_applies() {
    let core = in_memory_core();
    let mut inactive = percentage_off("PCT", 0, "SKU-A", 5_000);
    inactive.active = false;
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(10_000))
            .promoting(inactive),
    );

    let transaction = core.add_item(scan(BARCODE_A, 1)).unwrap();
    assert_eq!(total_discount(&transaction), 0);
}

#[test]
fn a_promotion_can_never_drive_a_line_or_the_sale_below_zero() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(1_000))
            .promoting(promotion(
                "OVER",
                0,
                Stackability::Stackable,
                CatalogScope::Tenant,
                PromotionRule::FixedAmountOff {
                    applies_to: applies_to("SKU-A"),
                    amount: Money::from_minor(500_000, Currency::Mxn),
                },
            )),
    );

    let transaction = core.add_item(scan(BARCODE_A, 1)).unwrap();
    let line = &transaction.state().lines[0];
    assert_eq!(line.discount_amount.minor(), 1_000);
    assert_eq!(line.line_total.minor(), 0);
    assert_eq!(line.taxable_amount.minor(), 0);
    assert_eq!(line.tax_amount.minor(), 0);

    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(totalized.state().totals.grand_total.minor(), 0);
}

// --------------------------------------------------------- tax interaction

#[test]
fn a_promotion_on_a_tax_inclusive_price_preserves_net_plus_tax() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(inclusive_product())
            .promoting(percentage_off("PCT-INC", 0, "SKU-INC", 2_500)),
    );

    // 116.00 inclusive, 25% off = 29.00, leaving 87.00 gross.
    let transaction = core.add_item(scan(BARCODE_INCLUSIVE, 1)).unwrap();
    let line = &transaction.state().lines[0];
    assert_eq!(line.price_mode, PriceMode::TaxInclusive);
    assert_eq!(line.discount_amount.minor(), 2_900);
    assert_eq!(line.line_total.minor(), 8_700);
    assert_eq!(line.taxable_amount.minor(), 7_500);
    assert_eq!(line.tax_amount.minor(), 1_200);
    assert_tax_identity(&transaction);
}

#[test]
fn an_allocated_basket_discount_still_balances_on_every_line() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            // Three awkward amounts, so the allocation cannot be exact.
            .with(product_a(3_333))
            .with(product_b(3_334))
            .with(inclusive_product())
            .promoting(basket_threshold("THR", 10_000, 1_001)),
    );
    core.add_item(scan(BARCODE_A, 1)).unwrap();
    core.add_item(scan(BARCODE_B, 1)).unwrap();
    core.add_item(scan(BARCODE_INCLUSIVE, 1)).unwrap();
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    // The allocation is exact: no cent is created or lost.
    assert_eq!(total_discount(&totalized), 1_001);
    assert_tax_identity(&totalized);
    let totals = &totalized.state().totals;
    assert_eq!(
        totals.net_total.minor() + totals.tax_total.minor(),
        totals.grand_total.minor()
    );
    assert_eq!(
        totals.subtotal.minor() - totals.discount_total.minor(),
        totalized
            .state()
            .lines
            .iter()
            .map(|line| if line.price_mode == PriceMode::TaxInclusive {
                line.line_total.minor()
            } else {
                line.taxable_amount.minor()
            })
            .sum::<i64>()
    );
}

// --------------------------------------------------------- snapshot binding

#[test]
fn a_sale_keeps_the_promotions_of_the_snapshot_it_captured() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(10_000))
            .promoting(percentage_off("PCT", 0, "SKU-A", 1_000)),
    );
    let first = core.add_item(scan(BARCODE_A, 1)).unwrap();
    assert_eq!(total_discount(&first), 1_000);

    // A newer snapshot drops the promotion entirely.
    CatalogFixture::new(2)
        .with(product_a(10_000))
        .activate(&core)
        .unwrap();

    // The open sale still prices against snapshot 1, promotions included.
    let second = core.add_item(scan(BARCODE_A, 1)).unwrap();
    assert_eq!(second.state().lines[1].discount_amount.minor(), 1_000);
    assert_eq!(total_discount(&second), 2_000);

    // A sale started after the switch gets the new snapshot's promotions,
    // which is to say none.
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.commit_cash_payment(CommitCashPaymentCommand {
        request_id: None,
        amount_minor: 100_000,
    })
    .unwrap();
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    let fresh = core.add_item(scan(BARCODE_A, 1)).unwrap();
    assert_eq!(total_discount(&fresh), 0);
}

#[test]
fn what_a_sale_granted_outlives_the_snapshot_that_granted_it() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(10_000))
            .promoting(percentage_off("PCT", 0, "SKU-A", 1_000)),
    );
    core.add_item(scan(BARCODE_A, 1)).unwrap();
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
    let transaction_id = finalized.id();

    // Abort-style purge: a fresh snapshot takes over and the old one is gone
    // as far as the sale is concerned. The materialised record still reads.
    CatalogFixture::new(2)
        .with(product_a(10_000))
        .activate(&core)
        .unwrap();

    let stored = core.get_transaction(transaction_id).unwrap().unwrap();
    let applied = stored.applied_promotions();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].code, "PCT");
    assert_eq!(applied[0].name, "Promoción PCT");
    assert_eq!(applied[0].amount.minor(), 1_000);
    assert_eq!(applied[0].reason, "PERCENTAGE_OFF");
    assert_eq!(stored.state().lines[0].discount_amount.minor(), 1_000);
}

// ----------------------------------------------------------------- journal

#[test]
fn the_promotion_summary_is_journalled_at_totalization_and_only_there() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(10_000))
            .promoting(percentage_off("PCT", 0, "SKU-A", 1_000)),
    );
    core.add_item(scan(BARCODE_A, 1)).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let journal = core.get_journal(50).unwrap();
    let with_summary: Vec<&JournalEvent> = journal
        .iter()
        .filter(|event| event.payload_json.get("promotions").is_some())
        .collect();
    assert_eq!(
        with_summary.len(),
        1,
        "exactly one event carries the summary"
    );
    assert_eq!(
        with_summary[0].event_type,
        JournalEventType::TransactionTotalized
    );
    let promotions = with_summary[0].payload_json["promotions"]
        .as_array()
        .unwrap();
    assert_eq!(promotions.len(), 1);
    assert_eq!(promotions[0]["code"], "PCT");
    assert_eq!(promotions[0]["amount_minor"], 1_000);
    assert_eq!(promotions[0]["target"], "LINE:1");
    assert_eq!(promotions[0]["adjustment_id"], "PCT#line:1");

    // The item event reports that promotions ran, without the money detail.
    let added = journal
        .iter()
        .find(|event| event.event_type == JournalEventType::ItemAdded)
        .unwrap();
    assert_eq!(added.payload_json["promotion_count"], 1);
    assert!(added.payload_json.get("promotions").is_none());
}

#[test]
fn removing_and_requantifying_lines_is_journalled() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(1_000))
            .with(product_b(1_000)),
    );
    core.add_item(scan(BARCODE_A, 1)).unwrap();
    core.add_item(scan(BARCODE_B, 1)).unwrap();
    core.change_quantity(ChangeQuantityCommand {
        request_id: None,
        line_number: 1,
        quantity: Quantity::from_units(4).unwrap(),
    })
    .unwrap();
    core.remove_item(RemoveItemCommand {
        request_id: None,
        line_number: 2,
    })
    .unwrap();

    let journal = core.get_journal(50).unwrap();
    let changed = journal
        .iter()
        .find(|event| event.event_type == JournalEventType::LineQuantityChanged)
        .expect("the quantity change is journalled");
    assert_eq!(changed.payload_json["from_quantity_thousandths"], 1_000);
    assert_eq!(changed.payload_json["to_quantity_thousandths"], 4_000);

    let removed = journal
        .iter()
        .find(|event| event.event_type == JournalEventType::ItemRemoved)
        .expect("the removal is journalled");
    assert_eq!(removed.payload_json["line_number"], 2);
    assert_eq!(removed.payload_json["remaining_lines"], 1);
}

// ------------------------------------------------------------- idempotency

#[test]
fn replaying_a_remove_or_a_quantity_change_applies_it_once() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(1_000))
            .with(product_b(1_000))
            .promoting(buy_two_get_one("BOGO", "SKU-A")),
    );
    core.add_item(scan(BARCODE_A, 3)).unwrap();
    core.add_item(scan(BARCODE_B, 1)).unwrap();

    let request_id = uuid::Uuid::from_u128(0xC0FFEE);
    let first = core
        .change_quantity(ChangeQuantityCommand {
            request_id: Some(request_id),
            line_number: 1,
            quantity: Quantity::from_units(6).unwrap(),
        })
        .unwrap();
    let replay = core
        .change_quantity(ChangeQuantityCommand {
            request_id: Some(request_id),
            line_number: 1,
            quantity: Quantity::from_units(6).unwrap(),
        })
        .unwrap();
    assert_eq!(first.state().version, replay.state().version);
    assert_eq!(total_discount(&replay), 2_000);

    let removal_id = uuid::Uuid::from_u128(0xBEEF);
    let removed = core
        .remove_item(RemoveItemCommand {
            request_id: Some(removal_id),
            line_number: 2,
        })
        .unwrap();
    let removed_again = core
        .remove_item(RemoveItemCommand {
            request_id: Some(removal_id),
            line_number: 2,
        })
        .unwrap();
    assert_eq!(removed.state().version, removed_again.state().version);
    assert_eq!(removed_again.state().lines.len(), 1);
}

// --------------------------------------------------------------- lifecycle

#[test]
fn a_basket_change_on_a_totalized_sale_reopens_it_and_voids_the_totals() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with(product_a(1_000))
            .with(product_b(1_000))
            .promoting(basket_threshold("THR", 1_500, 100)),
    );
    core.add_item(scan(BARCODE_A, 1)).unwrap();
    core.add_item(scan(BARCODE_B, 1)).unwrap();
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(totalized.state().status, TransactionStatus::Totalized);
    assert_eq!(total_discount(&totalized), 100);

    // Changing the basket drops the sale back to OPEN: the official totals no
    // longer describe it, so they are voided rather than left stale.
    let reopened = core
        .remove_item(RemoveItemCommand {
            request_id: None,
            line_number: 2,
        })
        .unwrap();
    assert_eq!(reopened.state().status, TransactionStatus::Open);
    assert_eq!(reopened.state().totals.grand_total.minor(), 0);
    // And the promotion is retracted, because the basket no longer earns it.
    assert_eq!(total_discount(&reopened), 0);

    // Paying now is refused until it is totalized again.
    assert_eq!(
        core.commit_cash_payment(CommitCashPaymentCommand {
            request_id: None,
            amount_minor: 2_000,
        })
        .unwrap_err()
        .kind(),
        "INVALID_TRANSACTION_STATE"
    );
}

#[test]
fn a_sale_in_payment_refuses_a_basket_change() {
    let core = in_memory_core();
    register_with(&core, CatalogFixture::new(1).with(product_a(1_000)));
    core.add_item(scan(BARCODE_A, 1)).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.commit_cash_payment(CommitCashPaymentCommand {
        request_id: None,
        amount_minor: 2_000,
    })
    .unwrap();

    // Once money has moved, the basket is closed to edits.
    assert_eq!(
        core.remove_item(RemoveItemCommand {
            request_id: None,
            line_number: 1,
        })
        .unwrap_err()
        .kind(),
        "INVALID_TRANSACTION_STATE"
    );
    assert_eq!(
        core.change_quantity(ChangeQuantityCommand {
            request_id: None,
            line_number: 1,
            quantity: Quantity::from_units(2).unwrap(),
        })
        .unwrap_err()
        .kind(),
        "INVALID_TRANSACTION_STATE"
    );
}
