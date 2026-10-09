//! A crash before COMMIT must leave no finalized sale without its journal and
//! outbox events — and no journal/outbox events without the sale.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::ports::EdgeStore;
use edge_crash_recovery_tests::*;
use edge_domain::prelude::*;

#[test]
fn finalize_is_all_or_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let core = core_at(&db, 1);
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    let committed = pay_cash(&core, 11_600);
    assert_eq!(committed.status(), TransactionStatus::Committed);

    // Simulate power loss after every write of the finalize use case but
    // before the SQLite COMMIT.
    core.store().arm_commit_failure();
    let result = core.finalize_transaction(FinalizeTransactionCommand::default());
    assert!(result.is_err(), "finalize should have failed");

    // Nothing from the aborted unit of work survived.
    let state = core.get_edge_state().unwrap();
    let active = state
        .active_transaction
        .expect("the sale must still be active");
    assert_eq!(active.status(), TransactionStatus::Committed);
    assert!(active.state().finalized_at.is_none());
    assert_eq!(state.pending_outbox, 0);
    assert!(!journal_types(&core).contains(&"TRANSACTION_FINALIZED".to_owned()));

    // Retrying after recovery succeeds, and now all three rows exist together.
    let finalized = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(finalized.status(), TransactionStatus::Finalized);
    assert_eq!(core.get_edge_state().unwrap().pending_outbox, 1);
    assert!(journal_types(&core).contains(&"TRANSACTION_FINALIZED".to_owned()));
}

#[test]
fn an_aborted_unit_of_work_writes_nothing_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let core = core_at(&db, 1);
    open_register(&core);

    let before = core.get_edge_state().unwrap();
    let outcome: Result<(), _> = core.store().transact(|uow| {
        // A plausible half-written unit of work: journal first, then failure.
        let device = uow.device_profile().unwrap().unwrap();
        uow.append_journal(&JournalEvent {
            event_id: EventId::from_uuid(uuid::Uuid::from_u128(0xDEAD)),
            event_type: JournalEventType::TransactionFinalized,
            aggregate_id: "ghost".to_owned(),
            tenant_id: device.tenant_id.clone(),
            store_id: device.store_id.clone(),
            register_id: None,
            business_day_id: None,
            session_id: None,
            transaction_id: None,
            operator_id: None,
            occurred_at: before.business_day.as_ref().unwrap().state().opened_at,
            payload_json: serde_json::json!({}),
        })?;
        Err(edge_application::AppError::Storage(
            "simulated crash".to_owned(),
        ))
    });
    assert!(outcome.is_err());
    assert!(!journal_types(&core).contains(&"TRANSACTION_FINALIZED".to_owned()));
}
