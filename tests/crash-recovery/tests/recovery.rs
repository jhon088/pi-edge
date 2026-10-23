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
        pay_cash(&core, 5_000);
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
    pay_cash(&core, 6_600);
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
        pay_cash(&core, 11_600);
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

/// A cancelled sale stays cancelled, and stops blocking the till.
///
/// The reason the use case exists: an abandoned sale would otherwise hold the
/// session close open forever, with no way out that does not involve money
/// (ADR 043).
#[test]
fn a_cancelled_sale_survives_a_restart_and_frees_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let cancelled_id = {
        let core = core_at(&db, 1);
        open_register(&core);
        core.start_transaction(StartTransactionCommand::default())
            .unwrap();
        core.add_item(demo_item()).unwrap();
        core.totalize_transaction(TotalizeTransactionCommand::default())
            .unwrap();
        let cancelled = core
            .cancel_transaction(CancelTransactionCommand {
                request_id: None,
                reason_code: Some("CUSTOMER_LEFT".to_owned()),
            })
            .unwrap();
        assert_eq!(cancelled.status(), TransactionStatus::Cancelled);
        cancelled.id()
    };

    let core = core_at(&db, 1_000);
    let state = core.get_edge_state().unwrap();
    assert!(
        state.active_transaction.is_none(),
        "a cancelled sale is not active"
    );
    assert!(state.session.is_some());
    // Nothing commercial happened, so there is nothing to report and nothing
    // to print.
    assert_eq!(state.pending_outbox, 0);
    assert_eq!(
        core.get_cash_position()
            .unwrap()
            .position
            .cash_sales
            .minor(),
        0
    );
    assert!(core.get_receipt(cancelled_id).is_err());

    // And the till can close, which was the whole point.
    core.request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    let closed = core
        .declare_closing_cash(DeclareClosingCashCommand {
            request_id: None,
            counted_minor: 100_000,
        })
        .unwrap();
    assert_eq!(closed.variance.map(|v| v.minor()), Some(0));
    core.close_business_day(CloseBusinessDayCommand::default())
        .unwrap();
}
