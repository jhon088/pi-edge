//! Refund state survives a process death.
//!
//! The rule every case here defends: **never give money back twice**. A refund
//! whose provider call was lost comes back unresolved and reconcilable, never
//! failed and never retried on its own (ADR 018, applied to refunds).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_cash::MovementType as CashMovementType;
use edge_crash_recovery_tests::*;
use edge_domain::prelude::*;
use edge_inventory::MovementType as InventoryMovementType;
use edge_refunds::{RefundFiscalStatus, RefundPaymentStatus, RefundStatus};

const SKU: &str = "SKU-001";
const UNIT: i64 = 11_600;

fn create(core: &TestCore, sale: TransactionId, lines: &[(i64, i64)]) -> RefundId {
    core.create_refund(CreateRefundCommand {
        request_id: None,
        authorization: central_authority(sale),
        lines: refund_lines(lines),
        reason_code: "CUSTOMER_RETURN".to_owned(),
        fiscal_action: RefundFiscalStatus::None,
    })
    .unwrap()
    .refund
    .refund_id
}

#[test]
fn a_created_refund_still_holds_its_quantities_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let sale;
    let refund_id;
    {
        let core = core_at(&db, 1);
        open_register(&core);
        bootstrap_stock(&core, &[(SKU, 10)]);
        sale = finalized_cash_sale(&core, UNIT);
        refund_id = create(&core, sale, &[(1, 1)]);
    }

    let core = core_at(&db, 1_000);
    let view = core.get_refund(refund_id).unwrap();
    assert_eq!(view.refund.status, RefundStatus::Created);
    assert_eq!(view.refund.total.gross.minor(), UNIT);
    assert!(!view.inventory_returned);

    // The reservation survived: a second refund of the same sale is still
    // refused, and the line still shows the quantity as spoken for (ADR 030).
    let eligibility = core.get_refund_eligibility(sale).unwrap();
    assert_eq!(eligibility.refund_in_progress, Some(refund_id));
    assert_eq!(eligibility.lines[0].refunded_thousandths, 1_000);
    assert_eq!(eligibility.lines[0].remaining_thousandths, 0);
}

#[test]
fn a_crash_before_commit_leaves_no_refund_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let core = core_at(&db, 1);
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, UNIT);

    core.store().arm_commit_failure();
    let command = CreateRefundCommand {
        request_id: Some(uuid::Uuid::from_u128(0xBEEF)),
        authorization: central_authority(sale),
        lines: refund_lines(&[(1, 1)]),
        reason_code: "CUSTOMER_RETURN".to_owned(),
        fiscal_action: RefundFiscalStatus::None,
    };
    assert!(core.create_refund(command.clone()).is_err());

    // No refund, no reservation, no journal event: the command never happened.
    let eligibility = core.get_refund_eligibility(sale).unwrap();
    assert!(eligibility.refund_in_progress.is_none());
    assert!(eligibility.refund_ids.is_empty());
    assert_eq!(eligibility.lines[0].remaining_thousandths, 1_000);
    assert!(!journal_types(&core).contains(&"REFUND_CREATED".to_owned()));

    // So the retry is a first attempt, not a refused replay.
    let retry = core.create_refund(command).unwrap();
    assert_eq!(retry.refund.status, RefundStatus::Created);
}

#[test]
fn an_accepted_refund_returns_the_goods_once_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let refund_id;
    {
        let core = core_at(&db, 1);
        open_register(&core);
        bootstrap_stock(&core, &[(SKU, 10)]);
        let sale = finalized_cash_sale(&core, UNIT);
        refund_id = create(&core, sale, &[(1, 1)]);
        core.accept_refund(AcceptRefundCommand {
            request_id: None,
            refund_id,
        })
        .unwrap();
        assert_eq!(on_hand(&core, SKU), 10_000);
    }

    // A client that lost the answer retries after the restart.
    let core = core_at(&db, 1_000);
    assert_eq!(
        core.accept_refund(AcceptRefundCommand {
            request_id: None,
            refund_id,
        })
        .unwrap_err()
        .kind(),
        "REFUND_INVALID_STATE"
    );

    assert_eq!(on_hand(&core, SKU), 10_000);
    assert_eq!(
        movements(&core, SKU)
            .iter()
            .filter(|m| m.movement_type == InventoryMovementType::RefundReturn)
            .count(),
        1
    );
}

#[test]
fn a_crash_before_commit_leaves_the_goods_where_they_were() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let core = core_at(&db, 1);
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, UNIT);
    let refund_id = create(&core, sale, &[(1, 1)]);

    core.store().arm_commit_failure();
    assert!(core
        .accept_refund(AcceptRefundCommand {
            request_id: None,
            refund_id,
        })
        .is_err());

    // The refund is still only created and the stock has not moved: there is
    // no state in which a refund is accepted and the goods are not back.
    assert_eq!(
        core.get_refund(refund_id).unwrap().refund.status,
        RefundStatus::Created
    );
    assert_eq!(on_hand(&core, SKU), 9_000);

    core.accept_refund(AcceptRefundCommand {
        request_id: None,
        refund_id,
    })
    .unwrap();
    assert_eq!(on_hand(&core, SKU), 10_000);
    assert_eq!(
        movements(&core, SKU)
            .iter()
            .filter(|m| m.movement_type == InventoryMovementType::RefundReturn)
            .count(),
        1
    );
}

#[test]
fn a_cash_refund_empties_the_drawer_once_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let refund_id;
    {
        let core = core_at(&db, 1);
        open_register_with_float(&core, 100_000);
        bootstrap_stock(&core, &[(SKU, 10)]);
        let sale = finalized_cash_sale(&core, UNIT);
        refund_id = create(&core, sale, &[(1, 1)]);
        core.accept_refund(AcceptRefundCommand {
            request_id: None,
            refund_id,
        })
        .unwrap();
        core.start_refund_payment(StartRefundPaymentCommand {
            request_id: None,
            refund_id,
        })
        .unwrap();
    }

    let core = core_at(&db, 1_000);
    // Retrying after the restart finds the refund already paid and gives
    // nothing back a second time.
    core.start_refund_payment(StartRefundPaymentCommand {
        request_id: None,
        refund_id,
    })
    .unwrap();

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
    assert_eq!(
        core.get_refund(refund_id).unwrap().refund.status,
        RefundStatus::Completed
    );
}

#[test]
fn a_fiscal_consequence_stays_pending_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let refund_id;
    {
        let core = core_at(&db, 1);
        open_register_with_float(&core, 100_000);
        bootstrap_stock(&core, &[(SKU, 10)]);
        let sale = finalized_cash_sale(&core, UNIT);
        core.mark_transaction_invoiced(MarkTransactionInvoicedCommand {
            request_id: None,
            transaction_id: sale,
            invoice_ref: "INV-0001".to_owned(),
        })
        .unwrap();
        refund_id = core
            .create_refund(CreateRefundCommand {
                request_id: None,
                authorization: central_authority(sale),
                lines: refund_lines(&[(1, 1)]),
                reason_code: "CUSTOMER_RETURN".to_owned(),
                fiscal_action: RefundFiscalStatus::CreditNotePending,
            })
            .unwrap()
            .refund
            .refund_id;
        core.accept_refund(AcceptRefundCommand {
            request_id: None,
            refund_id,
        })
        .unwrap();
        core.start_refund_payment(StartRefundPaymentCommand {
            request_id: None,
            refund_id,
        })
        .unwrap();
    }

    let core = core_at(&db, 1_000);
    let view = core.get_refund(refund_id).unwrap();
    // Commercially finished; the credit note is still owed, and a restart does
    // not decide it.
    assert_eq!(view.refund.status, RefundStatus::Completed);
    assert_eq!(
        view.refund.fiscal_status,
        RefundFiscalStatus::CreditNotePending
    );
    assert_eq!(
        view.refund.original_fiscal_state,
        edge_refunds::FiscalState::Invoiced
    );
}

#[test]
fn an_unresolved_card_refund_survives_a_restart_and_never_pays_twice() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let refund_id;
    let correlation_id;
    {
        // The refund goes out and the answer is lost.
        let core = scripted_core_at(
            &db,
            1,
            edge_payments::SimulatorScript::approving()
                .refunding(edge_payments::SimulatedOutcome::Unknown),
        );
        open_scripted_register(&core);
        bootstrap_stock_scripted(&core, &[(SKU, 10)]);
        let sale = card_sale(&core);
        refund_id = core
            .create_refund(CreateRefundCommand {
                request_id: None,
                authorization: central_authority(sale),
                lines: refund_lines(&[(1, 1)]),
                reason_code: "CUSTOMER_RETURN".to_owned(),
                fiscal_action: RefundFiscalStatus::None,
            })
            .unwrap()
            .refund
            .refund_id;
        core.accept_refund(AcceptRefundCommand {
            request_id: None,
            refund_id,
        })
        .unwrap();
        let view = core
            .start_refund_payment(StartRefundPaymentCommand {
                request_id: None,
                refund_id,
            })
            .unwrap();
        assert!(view.requires_reconciliation);
        assert_eq!(view.refund.status, RefundStatus::FinancialPending);
        correlation_id = view.payments[0].attempts[0].correlation_id;
    }

    // A new lifetime whose provider now confirms the refund.
    let core = scripted_core_at(
        &db,
        1_000,
        edge_payments::SimulatorScript::approving()
            .resolving_refund_queries_to(edge_payments::SimulatedOutcome::Approve),
    );
    let before = core.get_refund(refund_id).unwrap();
    // The restart decided nothing: still unresolved, still one attempt.
    assert_eq!(before.refund.status, RefundStatus::FinancialPending);
    assert_eq!(before.payments[0].attempts.len(), 1);
    assert_eq!(
        before.payments[0].payment.status,
        RefundPaymentStatus::Pending
    );

    let after = core
        .reconcile_refund(ReconcileRefundCommand {
            request_id: None,
            refund_id,
        })
        .unwrap();

    assert_eq!(after.refund.status, RefundStatus::Completed);
    assert_eq!(
        after.payments[0].payment.status,
        RefundPaymentStatus::Refunded
    );
    // The same attempt resolved: no second refund was ever sent.
    assert_eq!(after.payments[0].attempts.len(), 1);
    assert_eq!(after.payments[0].attempts[0].correlation_id, correlation_id);
    // And the goods stayed returned the whole time (ADR 033).
    assert_eq!(on_hand_scripted(&core, SKU), 10_000);
}
