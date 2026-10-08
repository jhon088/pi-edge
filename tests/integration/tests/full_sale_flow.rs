//! The vertical slice, end to end, through the real SQLite adapter.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_domain::prelude::*;
use edge_integration_tests::*;

#[test]
fn completes_a_cash_sale_and_closes_the_day() {
    let core = in_memory_core();
    open_register(&core);

    let transaction = core
        .start_transaction(StartTransactionCommand::default())
        .unwrap();
    assert_eq!(transaction.status(), TransactionStatus::Open);
    assert_eq!(transaction.state().register_sequence, 1);

    let transaction = core.add_item(demo_item()).unwrap();
    assert_eq!(transaction.state().lines.len(), 1);
    let line = &transaction.state().lines[0];
    assert_eq!(line.line_number, 1);
    assert_eq!(line.tax_amount.minor(), 1_600);
    assert_eq!(line.line_total.minor(), 11_600);

    let transaction = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(transaction.status(), TransactionStatus::Totalized);
    assert_eq!(transaction.state().totals.subtotal.minor(), 10_000);
    assert_eq!(transaction.state().totals.tax_total.minor(), 1_600);
    assert_eq!(transaction.state().totals.grand_total.minor(), 11_600);

    let transaction = core
        .commit_cash_payment(CommitCashPaymentCommand {
            request_id: None,
            amount_minor: 11_600,
        })
        .unwrap();
    assert_eq!(transaction.status(), TransactionStatus::Committed);
    assert_eq!(transaction.change_due().unwrap().minor(), 0);
    assert_eq!(transaction.balance_due().unwrap().minor(), 0);

    let finalized = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(finalized.status(), TransactionStatus::Finalized);
    assert!(finalized.state().finalized_at.is_some());

    // The sale is gone from the register, and the state query proves it was
    // rebuilt from SQLite.
    assert!(core.get_active_transaction().unwrap().is_none());
    let state = core.get_edge_state().unwrap();
    assert!(state.active_transaction.is_none());
    assert_eq!(state.pending_outbox, 1);

    // Journal: one structured event per operation, in order.
    assert_eq!(
        journal_types(&core),
        vec![
            "BUSINESS_DAY_OPENED",
            "SESSION_OPENED",
            "TRANSACTION_STARTED",
            "ITEM_ADDED",
            "TRANSACTION_TOTALIZED",
            "CASH_PAYMENT_COMMITTED",
            "TRANSACTION_FINALIZED",
        ]
    );

    // Outbox: the finalized sale, with the server-side idempotency key.
    let pending = core.get_pending_outbox(10).unwrap();
    assert_eq!(pending.pending_count, 1);
    assert_eq!(pending.events.len(), 1);
    let event = &pending.events[0];
    assert_eq!(event.event_type, OutboxEventType::SaleFinalized);
    assert_eq!(event.status, OutboxStatus::Pending);
    assert_eq!(event.attempt_count, 0);
    assert_eq!(event.aggregate_id, finalized.id().to_string());
    assert_eq!(
        event.payload_json["totals"]["grand_total"]["minor"],
        serde_json::json!(11_600)
    );

    core.close_register_session(CloseRegisterSessionCommand::default())
        .unwrap();
    let day = core
        .close_business_day(CloseBusinessDayCommand::default())
        .unwrap();
    assert_eq!(day.status(), BusinessDayStatus::Closed);

    let state = core.get_edge_state().unwrap();
    assert!(state.business_day.is_none());
    assert!(state.session.is_none());
    // Sale + session close + day close, none of them delivered yet.
    assert_eq!(state.pending_outbox, 3);
    assert_eq!(
        journal_types(&core).last().map(String::as_str),
        Some("BUSINESS_DAY_CLOSED")
    );
}

#[test]
fn overpayment_returns_change_computed_by_the_core() {
    let core = in_memory_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    let transaction = core
        .commit_cash_payment(CommitCashPaymentCommand {
            request_id: None,
            amount_minor: 20_000,
        })
        .unwrap();
    assert_eq!(transaction.change_due().unwrap().minor(), 8_400);
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
}

#[test]
fn splits_cash_payments_until_fully_paid() {
    let core = in_memory_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let partial = core
        .commit_cash_payment(CommitCashPaymentCommand {
            request_id: None,
            amount_minor: 5_000,
        })
        .unwrap();
    assert_eq!(partial.status(), TransactionStatus::PaymentInProgress);
    assert_eq!(partial.balance_due().unwrap().minor(), 6_600);

    let completed = core
        .commit_cash_payment(CommitCashPaymentCommand {
            request_id: None,
            amount_minor: 6_600,
        })
        .unwrap();
    assert_eq!(completed.status(), TransactionStatus::Committed);
    assert_eq!(completed.state().payments.len(), 2);
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
}

#[test]
fn register_sequence_increments_per_register() {
    let core = in_memory_core();
    open_register(&core);
    for expected_sequence in 1..=3 {
        let transaction = core
            .start_transaction(StartTransactionCommand::default())
            .unwrap();
        assert_eq!(transaction.state().register_sequence, expected_sequence);
        core.add_item(demo_item()).unwrap();
        core.totalize_transaction(TotalizeTransactionCommand::default())
            .unwrap();
        core.commit_cash_payment(CommitCashPaymentCommand {
            request_id: None,
            amount_minor: 11_600,
        })
        .unwrap();
        core.finalize_transaction(FinalizeTransactionCommand::default())
            .unwrap();
    }
    let pending = core.get_pending_outbox(10).unwrap();
    assert_eq!(pending.pending_count, 3);
}
