//! Commercial refunds against real SQLite.
//!
//! The refundable arithmetic is pinned by unit tests in `edge-refunds`; these
//! check what only the whole runtime shows: that eligibility comes from the
//! sale that actually happened, that partial refunds exhaust a line exactly,
//! that the goods come back once and the money goes out once, that an
//! unresolved card refund never pays twice, and that the fiscal consequence is
//! constrained by the original sale.
//!
//! The scenarios mirror the ones the previous Flutter POS had already
//! validated — total refund, partial refund, invoiced and non-invoiced
//! originals, credit note pending and requested, cash and card, inventory
//! return, replay — turned into executable requirements here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_cash::MovementType as CashMovementType;
use edge_domain::prelude::*;
use edge_integration_tests::*;
use edge_inventory::MovementType as InventoryMovementType;
use edge_payments::{SimulatorScript, TenderType};
use edge_refunds::{RefundFiscalStatus, RefundStatus};

/// The demo product: 100.00 at 16% = 116.00 for one unit.
const SKU: &str = "SKU-001";
const UNIT: i64 = 11_600;

fn refund_of(core: &TestCore, transaction_id: TransactionId, lines: &[(i64, i64)]) -> RefundId {
    create_refund(core, transaction_id, refund_lines(lines))
        .unwrap()
        .refund
        .refund_id
}

// ------------------------------------------------------------- eligibility

#[test]
fn only_a_finalized_sale_can_be_refunded() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);

    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    let open = core.get_active_transaction().unwrap().unwrap();

    let view = core.get_refund_eligibility(open.id()).unwrap();
    assert_eq!(view.original_status, "OPEN");

    let error = create_refund(&core, open.id(), refund_lines(&[(1, 1)])).unwrap_err();
    // A sale still being built has charged nothing and owes nothing back.
    assert_eq!(error.kind(), "REFUND_ORIGINAL_NOT_REFUNDABLE");
}

#[test]
fn an_unknown_sale_cannot_be_refunded() {
    let core = in_memory_core();
    open_register(&core);
    let ghost = TransactionId::from_uuid(uuid::Uuid::from_u128(0xDEAD));

    assert_eq!(
        core.get_refund_eligibility(ghost).unwrap_err().kind(),
        "TRANSACTION_NOT_FOUND"
    );
}

/// The domain receives an already-resolved authority and materialises it, so
/// an audit can see *who* said the sale was refundable. Refusing a refund for
/// want of authority happens where authority is resolved — the composition
/// root — and is pinned in the daemon suite (ADR 030).
#[test]
fn the_resolved_authority_is_materialised_on_the_refund() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, demo_item(), UNIT);

    let view = create_refund(&core, sale, refund_lines(&[(1, 1)])).unwrap();
    assert_eq!(
        view.refund.authority,
        edge_refunds::RefundAuthority::CentralAccepted
    );
    assert_eq!(
        view.refund.authorization_ref.as_deref(),
        Some(format!("TEST-CENTRAL:{sale}").as_str())
    );
}

#[test]
fn eligibility_reports_what_each_line_and_each_tender_can_still_give_back() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, item(SKU, 3), 40_000);

    let view = core.get_refund_eligibility(sale).unwrap();
    assert_eq!(view.original_status, "FINALIZED");
    assert_eq!(view.lines.len(), 1);
    let line = &view.lines[0];
    assert_eq!(line.sold_quantity, Quantity::from_units(3).unwrap());
    assert_eq!(line.refunded_thousandths, 0);
    assert_eq!(line.remaining_thousandths, 3_000);
    assert_eq!(line.remaining.gross.minor(), 3 * UNIT);
    assert_eq!(view.refundable_total.minor(), 3 * UNIT);

    // The tender ceiling is what that tender actually applied, never what the
    // customer handed over (ADR 031).
    assert_eq!(view.tenders.len(), 1);
    assert_eq!(view.tenders[0].tender_type, "CASH");
    assert_eq!(view.tenders[0].committed.minor(), 3 * UNIT);
    assert_eq!(view.tenders[0].remaining.minor(), 3 * UNIT);
    assert!(view.refund_in_progress.is_none());
    assert!(view.refund_ids.is_empty());
}

#[test]
fn eligibility_shrinks_as_refunds_consume_the_lines() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, item(SKU, 3), 40_000);

    full_refund(&core, sale, refund_lines(&[(1, 1)]));
    let after_one = core.get_refund_eligibility(sale).unwrap();
    assert_eq!(after_one.lines[0].remaining_thousandths, 2_000);
    assert_eq!(after_one.refundable_total.minor(), 2 * UNIT);
    assert_eq!(after_one.refund_ids.len(), 1);

    full_refund(&core, sale, refund_lines(&[(1, 2)]));
    let exhausted = core.get_refund_eligibility(sale).unwrap();
    assert_eq!(exhausted.lines[0].remaining_thousandths, 0);
    assert_eq!(exhausted.refundable_total.minor(), 0);
    assert!(exhausted.lines[0].is_exhausted());

    // A third refund has nothing left to take.
    let error = create_refund(&core, sale, refund_lines(&[(1, 1)])).unwrap_err();
    assert_eq!(error.kind(), "REFUND_QUANTITY_EXCEEDED");
}

#[test]
fn refunding_more_than_was_sold_is_refused() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, item(SKU, 2), 30_000);

    let error = create_refund(&core, sale, refund_lines(&[(1, 3)])).unwrap_err();
    assert_eq!(error.kind(), "REFUND_QUANTITY_EXCEEDED");
    // And nothing was reserved by the attempt.
    assert!(core
        .get_refund_eligibility(sale)
        .unwrap()
        .refund_in_progress
        .is_none());
}

#[test]
fn a_line_the_sale_does_not_have_is_refused() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, demo_item(), UNIT);

    let error = create_refund(&core, sale, refund_lines(&[(7, 1)])).unwrap_err();
    assert_eq!(error.kind(), "REFUND_LINE_NOT_FOUND");
}

#[test]
fn a_refund_has_to_name_a_line_and_a_reason() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, demo_item(), UNIT);

    assert_eq!(
        create_refund(&core, sale, Vec::new()).unwrap_err().kind(),
        "REFUND_INVALID_LINES"
    );
    let blank = core
        .create_refund(CreateRefundCommand {
            request_id: None,
            authorization: central_authority(sale),
            lines: refund_lines(&[(1, 1)]),
            reason_code: "   ".to_owned(),
            fiscal_action: RefundFiscalStatus::None,
        })
        .unwrap_err();
    assert_eq!(blank.kind(), "REFUND_REASON_REQUIRED");
}

#[test]
fn the_same_line_cannot_appear_twice_in_one_refund() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, item(SKU, 3), 40_000);

    let error = create_refund(&core, sale, refund_lines(&[(1, 1), (1, 1)])).unwrap_err();
    assert_eq!(error.kind(), "REFUND_INVALID_LINES");
}

// ------------------------------------------------------------- the money

#[test]
fn a_total_refund_gives_back_exactly_what_the_sale_charged() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, item(SKU, 2), 30_000);

    let view = full_refund(&core, sale, refund_lines(&[(1, 2)]));
    assert_eq!(view.refund.total.gross.minor(), 2 * UNIT);
    assert_eq!(view.refund.total.net.minor(), 20_000);
    assert_eq!(view.refund.total.tax.minor(), 3_200);
    // net + tax is what goes back, always.
    assert_eq!(
        view.refund
            .total
            .net
            .checked_add(view.refund.total.tax)
            .unwrap(),
        view.refund.total.gross
    );
    assert_eq!(view.refund.status, RefundStatus::Completed);
}

#[test]
fn partial_refunds_of_a_promotional_line_never_exceed_what_was_paid() {
    let core = in_memory_core();
    init_device(&core);
    promotional_catalog(1).activate(&core).unwrap();
    open_day_and_session(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);

    // Buy 2 get 1: three units, 232.00 charged.
    let sale = finalized_cash_sale(&core, item(SKU, 3), 30_000);
    let charged = core
        .get_refund_eligibility(sale)
        .unwrap()
        .original_total
        .minor();
    assert_eq!(charged, 23_200);

    let mut given_back = 0;
    for units in [1, 1, 1] {
        let view = full_refund(&core, sale, refund_lines(&[(1, units)]));
        given_back += view.refund.total.gross.minor();
    }

    // Exactly what the customer paid. Not 3 x 116.00, which would be
    // arbitrage, and not less, which would short the customer.
    assert_eq!(given_back, charged);
    assert_eq!(
        core.get_refund_eligibility(sale)
            .unwrap()
            .refundable_total
            .minor(),
        0
    );
}

#[test]
fn a_fractional_refund_gives_back_its_share() {
    let core = in_memory_core();
    init_device(&core);
    pricing_catalog(1).activate(&core).unwrap();
    open_day_and_session(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);

    // 1.000 of the demo product at 100.00 + 16%.
    let sale = finalized_cash_sale(&core, item(SKU, 1), 20_000);
    let view = full_refund(&core, sale, refund_lines_thousandths(&[(1, 250)]));

    assert_eq!(view.refund.total.net.minor(), 2_500);
    assert_eq!(view.refund.total.tax.minor(), 400);
    assert_eq!(view.refund.total.gross.minor(), 2_900);
    assert_eq!(
        core.get_refund_eligibility(sale).unwrap().lines[0].remaining_thousandths,
        750
    );
}

#[test]
fn a_refund_is_priced_from_the_sale_not_from_todays_catalog() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, demo_item(), UNIT);

    // The catalog changes after the sale: a new snapshot at a new price.
    edge_application::testing::CatalogFixture::demo(2)
        .activate(&core)
        .unwrap();

    let view = full_refund(&core, sale, refund_lines(&[(1, 1)]));
    // Still 116.00: the refund consumes the economics the sale materialised,
    // so the new price is irrelevant (ADR 031).
    assert_eq!(view.refund.total.gross.minor(), UNIT);
}

// -------------------------------------------------------- the tender spread

#[test]
fn a_mixed_tender_refund_spreads_pro_rata_over_the_original_tenders() {
    let core = scripted_core(SimulatorScript::approving());
    open_scripted_register(&core);

    // Ten units at 116.00 = 1160.00, settled 300.00 cash + 860.00 card.
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(item(SKU, 10)).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    let due = core
        .get_active_transaction()
        .unwrap()
        .unwrap()
        .state()
        .totals
        .grand_total
        .minor();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Cash,
        amount_minor: 30_000,
    })
    .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Card,
        amount_minor: due - 30_000,
    })
    .unwrap();
    let sale = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap()
        .id();

    // Refund half the sale: each tender gives back its own proportion.
    let refund = core
        .create_refund(CreateRefundCommand {
            request_id: None,
            authorization: central_authority(sale),
            lines: refund_lines(&[(1, 5)]),
            reason_code: "CUSTOMER_RETURN".to_owned(),
            fiscal_action: RefundFiscalStatus::None,
        })
        .unwrap();
    core.accept_refund(AcceptRefundCommand {
        request_id: None,
        refund_id: refund.refund.refund_id,
    })
    .unwrap();
    let paid = core
        .start_refund_payment(StartRefundPaymentCommand {
            request_id: None,
            refund_id: refund.refund.refund_id,
        })
        .unwrap();

    let total: i64 = paid
        .payments
        .iter()
        .map(|view| view.payment.requested_amount.minor())
        .sum();
    assert_eq!(total, refund.refund.total.gross.minor());
    assert_eq!(paid.payments.len(), 2);
    let cash = paid
        .payments
        .iter()
        .find(|view| view.payment.tender_type == TenderType::Cash)
        .unwrap();
    let card = paid
        .payments
        .iter()
        .find(|view| view.payment.tender_type == TenderType::Card)
        .unwrap();
    // Cash covered 300.00 of 1160.00, so it gives back the same proportion of
    // the 580.00 refund: 150.00. The card gives back the other 430.00.
    assert_eq!(refund.refund.total.gross.minor(), 58_000);
    assert_eq!(cash.payment.requested_amount.minor(), 15_000);
    assert_eq!(card.payment.requested_amount.minor(), 43_000);
    // And neither tender is asked for more than it took.
    assert!(cash.payment.requested_amount.minor() <= 30_000);
    assert!(card.payment.requested_amount.minor() <= due - 30_000);
    assert_eq!(paid.refund.status, RefundStatus::Completed);
}

// ----------------------------------------------------------------- the cash

#[test]
fn a_cash_refund_takes_the_money_out_of_the_drawer_once() {
    let core = in_memory_core();
    open_register_with_float(&core, 100_000);
    activate_demo_catalog(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, demo_item(), UNIT);

    let before = core.get_cash_position().unwrap().position.expected_cash;
    full_refund(&core, sale, refund_lines(&[(1, 1)]));
    let after = core.get_cash_position().unwrap();

    assert_eq!(after.position.cash_refunds.minor(), UNIT);
    assert_eq!(after.position.expected_cash.minor(), before.minor() - UNIT);
    let refunds: Vec<_> = after
        .movements
        .iter()
        .filter(|movement| movement.movement_type == CashMovementType::CashRefund)
        .collect();
    assert_eq!(refunds.len(), 1);
    assert_eq!(refunds[0].direction, edge_cash::Direction::Out);
    assert_eq!(refunds[0].amount.minor(), UNIT);
}

#[test]
fn a_replayed_refund_payment_empties_the_drawer_once() {
    let core = in_memory_core();
    open_register_with_float(&core, 100_000);
    activate_demo_catalog(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, demo_item(), UNIT);
    let refund_id = refund_of(&core, sale, &[(1, 1)]);
    core.accept_refund(AcceptRefundCommand {
        request_id: None,
        refund_id,
    })
    .unwrap();

    let request_id = uuid::Uuid::new_v4();
    for _ in 0..2 {
        core.start_refund_payment(StartRefundPaymentCommand {
            request_id: Some(request_id),
            refund_id,
        })
        .unwrap();
    }

    let position = core.get_cash_position().unwrap();
    assert_eq!(position.position.cash_refunds.minor(), UNIT);
    assert_eq!(
        position
            .movements
            .iter()
            .filter(|m| m.movement_type == CashMovementType::CashRefund)
            .count(),
        1
    );
}

// ------------------------------------------------------------ the inventory

#[test]
fn accepting_a_refund_returns_the_goods_once() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, item(SKU, 2), 30_000);
    assert_eq!(on_hand(&core, SKU), 8_000);

    let refund_id = refund_of(&core, sale, &[(1, 2)]);
    // Creating a refund moves nothing: the goods are not back yet (ADR 033).
    assert_eq!(on_hand(&core, SKU), 8_000);

    let request_id = uuid::Uuid::new_v4();
    for _ in 0..2 {
        core.accept_refund(AcceptRefundCommand {
            request_id: Some(request_id),
            refund_id,
        })
        .unwrap();
    }

    assert_eq!(on_hand(&core, SKU), 10_000);
    let movements = position(&core, SKU).movements;
    let returns: Vec<_> = movements
        .iter()
        .filter(|m| m.movement_type == InventoryMovementType::RefundReturn)
        .collect();
    assert_eq!(returns.len(), 1);
    assert_eq!(returns[0].direction, edge_inventory::Direction::In);
    assert_eq!(returns[0].quantity, Quantity::from_units(2).unwrap());
    assert_eq!(returns[0].source_ref, format!("{refund_id}:1"));
    // Never a manual movement: a refund return says where the stock came from.
    assert!(!movements
        .iter()
        .any(|m| m.movement_type == InventoryMovementType::ManualIn));
}

#[test]
fn a_promotional_refund_returns_the_physical_quantity() {
    let core = in_memory_core();
    init_device(&core);
    promotional_catalog(1).activate(&core).unwrap();
    open_day_and_session(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, item(SKU, 3), 30_000);
    assert_eq!(on_hand(&core, SKU), 7_000);

    full_refund(&core, sale, refund_lines(&[(1, 3)]));

    // Three units left the store and three come back, whatever the promotion
    // did to the money.
    assert_eq!(on_hand(&core, SKU), 10_000);
}

#[test]
fn a_refund_that_never_took_the_goods_moves_no_stock() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, demo_item(), UNIT);
    let refund_id = refund_of(&core, sale, &[(1, 1)]);

    // The refund exists, reserved, and nothing physical happened.
    assert_eq!(on_hand(&core, SKU), 9_000);
    assert!(!core.get_refund(refund_id).unwrap().inventory_returned);
    assert_eq!(
        position(&core, SKU)
            .movements
            .iter()
            .filter(|m| m.movement_type == InventoryMovementType::RefundReturn)
            .count(),
        0
    );
}

// --------------------------------------------------------------- the fiscal

#[test]
fn a_sale_that_was_never_invoiced_admits_only_no_credit_note() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, demo_item(), UNIT);

    // Every finalized sale carries its fiscal metadata, so this is a fact and
    // not an absence (ADR 032).
    assert_eq!(
        core.get_refund_eligibility(sale).unwrap().fiscal_state,
        edge_refunds::FiscalState::NotInvoiced
    );

    for attempted in [
        RefundFiscalStatus::CreditNotePending,
        RefundFiscalStatus::CreditNoteRequested,
    ] {
        let error = core
            .create_refund(CreateRefundCommand {
                request_id: None,
                authorization: central_authority(sale),
                lines: refund_lines(&[(1, 1)]),
                reason_code: "CUSTOMER_RETURN".to_owned(),
                fiscal_action: attempted,
            })
            .unwrap_err();
        assert_eq!(error.kind(), "REFUND_FISCAL_ACTION_INVALID");
    }

    let view = create_refund(&core, sale, refund_lines(&[(1, 1)])).unwrap();
    assert_eq!(view.refund.fiscal_status, RefundFiscalStatus::None);
}

#[test]
fn an_invoiced_sale_has_to_choose_its_credit_note_consequence() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, item(SKU, 2), 30_000);
    mark_invoiced(&core, sale);

    let view = core.get_refund_eligibility(sale).unwrap();
    assert_eq!(view.fiscal_state, edge_refunds::FiscalState::Invoiced);
    assert_eq!(view.invoice_ref.as_deref(), Some("INV-0001"));

    // Dropping the credit note of an invoiced sale leaves the books wrong.
    let error = create_refund(&core, sale, refund_lines(&[(1, 1)])).unwrap_err();
    assert_eq!(error.kind(), "REFUND_FISCAL_ACTION_INVALID");

    let pending = core
        .create_refund(CreateRefundCommand {
            request_id: None,
            authorization: central_authority(sale),
            lines: refund_lines(&[(1, 1)]),
            reason_code: "CUSTOMER_RETURN".to_owned(),
            fiscal_action: RefundFiscalStatus::CreditNotePending,
        })
        .unwrap();
    assert_eq!(
        pending.refund.fiscal_status,
        RefundFiscalStatus::CreditNotePending
    );
    // The original's fiscal state is materialised on the refund, so a later
    // invoice cannot rewrite the history of this decision.
    assert_eq!(
        pending.refund.original_fiscal_state,
        edge_refunds::FiscalState::Invoiced
    );
}

#[test]
fn a_commercial_refund_completes_with_its_credit_note_still_pending() {
    let core = in_memory_core();
    open_register_with_float(&core, 100_000);
    activate_demo_catalog(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, demo_item(), UNIT);
    mark_invoiced(&core, sale);

    let refund = core
        .create_refund(CreateRefundCommand {
            request_id: None,
            authorization: central_authority(sale),
            lines: refund_lines(&[(1, 1)]),
            reason_code: "CUSTOMER_RETURN".to_owned(),
            fiscal_action: RefundFiscalStatus::CreditNotePending,
        })
        .unwrap();
    core.accept_refund(AcceptRefundCommand {
        request_id: None,
        refund_id: refund.refund.refund_id,
    })
    .unwrap();
    let done = core
        .start_refund_payment(StartRefundPaymentCommand {
            request_id: None,
            refund_id: refund.refund.refund_id,
        })
        .unwrap();

    // Commercially finished, fiscally owed. The two settle separately, and
    // PI.Edge issues no CFDI either way (ADR 032).
    assert_eq!(done.refund.status, RefundStatus::Completed);
    assert_eq!(
        done.refund.fiscal_status,
        RefundFiscalStatus::CreditNotePending
    );
    assert_eq!(on_hand(&core, SKU), 10_000);
    assert_eq!(
        core.get_cash_position()
            .unwrap()
            .position
            .cash_refunds
            .minor(),
        UNIT
    );
}

// ------------------------------------------------------- concurrency, replay

#[test]
fn one_refund_at_a_time_per_sale() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, item(SKU, 3), 40_000);

    let first = refund_of(&core, sale, &[(1, 1)]);
    let error = create_refund(&core, sale, refund_lines(&[(1, 1)])).unwrap_err();
    assert_eq!(error.kind(), "REFUND_IN_PROGRESS");

    // Once the first finishes, a second may take what is left.
    core.accept_refund(AcceptRefundCommand {
        request_id: None,
        refund_id: first,
    })
    .unwrap();
    core.start_refund_payment(StartRefundPaymentCommand {
        request_id: None,
        refund_id: first,
    })
    .unwrap();
    assert!(create_refund(&core, sale, refund_lines(&[(1, 2)])).is_ok());
}

#[test]
fn a_replayed_create_returns_the_same_refund() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, item(SKU, 3), 40_000);

    let request_id = uuid::Uuid::new_v4();
    let command = || CreateRefundCommand {
        request_id: Some(request_id),
        authorization: central_authority(sale),
        lines: refund_lines(&[(1, 1)]),
        reason_code: "CUSTOMER_RETURN".to_owned(),
        fiscal_action: RefundFiscalStatus::None,
    };
    let first = core.create_refund(command()).unwrap();
    let replay = core.create_refund(command()).unwrap();

    assert_eq!(first.refund.refund_id, replay.refund.refund_id);
    // One refund, and the line still has two units left, not one.
    let view = core.get_refund_eligibility(sale).unwrap();
    assert_eq!(view.refund_ids.len(), 1);
    assert_eq!(view.lines[0].remaining_thousandths, 2_000);
}

#[test]
fn the_journal_records_the_commercial_transitions() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, demo_item(), UNIT);
    full_refund(&core, sale, refund_lines(&[(1, 1)]));

    let types = journal_types(&core);
    for expected in [
        "REFUND_CREATED",
        "REFUND_ACCEPTED",
        "REFUND_FINANCIAL_STARTED",
    ] {
        assert_eq!(
            types.iter().filter(|t| t.as_str() == expected).count(),
            1,
            "{expected}"
        );
    }
    // The ledgers carry the physical detail; the journal does not repeat it.
    assert_eq!(
        types
            .iter()
            .filter(|t| t.as_str() == "REFUND_FINANCIAL_COMPLETED")
            .count(),
        0,
        "a cash refund settles without a provider, so there is no outcome event"
    );
}

#[test]
fn a_refund_is_not_a_payment_reversal() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, demo_item(), UNIT);
    full_refund(&core, sale, refund_lines(&[(1, 1)]));

    // The original payment is untouched: still committed, not reversed. A
    // refund is a new document, not a correction of the sale (ADR 034).
    let (payments, settlement) = core.get_transaction_payments(sale).unwrap();
    assert_eq!(payments.len(), 1);
    assert_eq!(
        payments[0].payment.status,
        edge_payments::PaymentStatus::Committed
    );
    assert_eq!(settlement.committed_total.minor(), UNIT);
    // And the sale is still finalized, not reopened.
    let original = core.get_refund_eligibility(sale).unwrap();
    assert_eq!(original.original_status, "FINALIZED");
}
