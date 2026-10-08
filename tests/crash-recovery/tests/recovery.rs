//! Kill the application object, rebuild it, and require the exact same state.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_crash_recovery_tests::*;
use edge_domain::prelude::*;

#[test]
fn recovers_an_in_flight_sale_after_the_process_dies() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    // --- lifetime 1: a sale is half paid when the lights go out -------------
    let (expected_state, expected_journal) = {
        let core = core_at(&db, 1);
        open_register(&core);
        core.start_transaction(StartTransactionCommand::default())
            .unwrap();
        core.add_item(demo_item()).unwrap();
        core.totalize_transaction(TotalizeTransactionCommand::default())
            .unwrap();
        core.commit_cash_payment(CommitCashPaymentCommand {
            request_id: None,
            amount_minor: 5_000,
        })
        .unwrap();
        let state = core.get_edge_state().unwrap();
        let journal = journal_types(&core);
        // The application object (and its connection) is dropped here.
        (state, journal)
    };

    assert_eq!(
        expected_state
            .active_transaction
            .as_ref()
            .map(RetailTransaction::status),
        Some(TransactionStatus::PaymentInProgress)
    );

    // --- lifetime 2: a brand new core reads SQLite --------------------------
    let core = core_at(&db, 1_000);
    let recovered = core.get_edge_state().unwrap();

    assert_eq!(recovered.device, expected_state.device);
    assert_eq!(recovered.business_day, expected_state.business_day);
    assert_eq!(recovered.session, expected_state.session);
    assert_eq!(
        recovered.active_transaction,
        expected_state.active_transaction
    );
    assert_eq!(recovered.pending_outbox, 0);
    assert_eq!(journal_types(&core), expected_journal);

    // ... and the recovered sale can still be completed.
    let transaction = recovered.active_transaction.unwrap();
    assert_eq!(transaction.balance_due().unwrap().minor(), 6_600);
    core.commit_cash_payment(CommitCashPaymentCommand {
        request_id: None,
        amount_minor: 6_600,
    })
    .unwrap();
    let finalized = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(finalized.status(), TransactionStatus::Finalized);
    assert_eq!(finalized.id(), transaction.id());
    assert_eq!(finalized.state().register_sequence, 1);
    assert_eq!(core.get_edge_state().unwrap().pending_outbox, 1);
}

#[test]
fn a_finalized_sale_stays_finalized_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let transaction_id = {
        let core = core_at(&db, 1);
        open_register(&core);
        core.start_transaction(StartTransactionCommand::default())
            .unwrap();
        core.add_item(demo_item()).unwrap();
        core.totalize_transaction(TotalizeTransactionCommand::default())
            .unwrap();
        core.commit_cash_payment(CommitCashPaymentCommand {
            request_id: None,
            amount_minor: 11_600,
        })
        .unwrap();
        core.finalize_transaction(FinalizeTransactionCommand::default())
            .unwrap()
            .id()
    };

    let core = core_at(&db, 1_000);
    let state = core.get_edge_state().unwrap();
    assert!(state.active_transaction.is_none());
    assert!(state.business_day.is_some());
    assert!(state.session.is_some());
    assert_eq!(state.pending_outbox, 1);

    let pending = core.get_pending_outbox(10).unwrap();
    assert_eq!(pending.events[0].aggregate_id, transaction_id.to_string());
    assert_eq!(
        pending.events[0].payload_json["status"],
        serde_json::json!("FINALIZED")
    );

    // A second sale on the same register continues the sequence.
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    let next = core.get_active_transaction().unwrap().unwrap();
    assert_eq!(next.state().register_sequence, 2);
}
