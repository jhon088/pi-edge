//! What a process death does to a dispatch in flight.
//!
//! Two windows matter, and both are opened here with a real injected commit
//! failure rather than a story about one:
//!
//! * **Outbound.** The claim committed, the server answered, and the process
//!   died before the answer could be written. The fact is left `SENDING`, which
//!   is not a resting state: recovery queues it again with the same `event_id`
//!   and the consumer deduplicates the second delivery (ADR 041).
//! * **Inbound.** The effect committed and the process died before the cursor
//!   moved. The message is delivered again, which is harmless because every
//!   inbound message is idempotent on something the core already had — and far
//!   better than the other order, where a cursor ahead of local state would
//!   skip the message forever.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use edge_application::commands::*;
use edge_application::queries::SyncCycleOutcome;
use edge_application::testing::CatalogFixture;
use edge_crash_recovery_tests::*;
use edge_domain::prelude::*;
use edge_storage::SqliteEdgeStore;
use edge_sync::simulator::{SimulatorBehavior, SimulatorSyncTransport};

fn transport() -> Arc<SimulatorSyncTransport> {
    Arc::new(SimulatorSyncTransport::new())
}

fn a_finalized_sale(core: &TestCore) {
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(core, 11_600);
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
}

#[test]
fn a_fact_the_server_acknowledged_is_resent_when_the_answer_was_never_written() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let remote = transport();

    let delivered;
    {
        let core = syncing_core_at(&db, 1, &remote);
        open_register(&core);
        a_finalized_sale(&core);

        // A cycle commits the claim first and the answer second. Letting the
        // first one through and killing the second is exactly the window that
        // matters: the server has the fact, PI.Edge never recorded that it
        // does — the sync side of ADR 020.
        core.store().arm_commit_failure_after(1);
        let outcome = core.run_sync_cycle();
        assert!(outcome.is_err(), "the settle transaction must have failed");

        delivered = remote.deliveries();
        assert_eq!(delivered.len(), 1, "the server does have the fact");

        // Locally it is still in flight, because nothing recorded otherwise.
        let sending = outbox_entries(&core, OutboxStatus::Sending);
        assert_eq!(sending.len(), 1);
        assert_eq!(sending[0].event.attempt_count, 1);
    }

    // A new runtime on the same installation. One writer, so a row still
    // claimed means nobody is coming back for it.
    let core = syncing_core_at(&db, 10_000, &remote);
    assert_eq!(core.recover_interrupted_sync().unwrap(), 1);
    let pending = outbox_entries(&core, OutboxStatus::Pending);
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].event.event_id.to_uuid(),
        delivered[0],
        "the same id the server already holds"
    );
    assert_eq!(
        pending[0].last_error_code.as_deref(),
        Some("SYNC_INTERRUPTED"),
        "an operator can see why it is back in the queue"
    );

    // The resend is a duplicate: two deliveries, one logical fact, one SENT.
    let report = core.run_sync_cycle().unwrap();
    assert_eq!(report.acknowledged, 1);
    assert_eq!(remote.delivery_count(delivered[0]), 2);
    assert_eq!(remote.distinct_facts_received(), 1);
    let status = core.get_sync_status().unwrap();
    assert_eq!(status.outbox.sent, 1);
    assert_eq!(status.outbox.sending, 0);
    assert_eq!(status.outbox.pending, 0);
}

#[test]
fn recovery_runs_even_when_no_transport_is_configured() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    {
        let remote = transport();
        let core = syncing_core_at(&db, 1, &remote);
        open_register(&core);
        a_finalized_sale(&core);
        core.store().arm_commit_failure_after(1);
        assert!(core.run_sync_cycle().is_err());
        assert_eq!(outbox_entries(&core, OutboxStatus::Sending).len(), 1);
    }

    // The installation comes back with sync switched off. The stuck row is
    // still local state and is still unstuck: `SENDING` is never where a fact
    // is allowed to sit.
    let core = core_at(&db, 10_000);
    assert_eq!(core.recover_interrupted_sync().unwrap(), 1);
    let status = core.get_sync_status().unwrap();
    assert_eq!(status.outbox.sending, 0);
    assert_eq!(status.outbox.pending, 1);
    assert_eq!(status.driver, edge_sync::unconfigured::UNCONFIGURED);
    // And it waits there, indefinitely and safely.
    assert_eq!(
        core.run_sync_cycle().unwrap().outcome,
        SyncCycleOutcome::NotConfigured
    );
}

#[test]
fn an_inbound_effect_survives_a_crash_before_its_cursor_moved() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let remote = transport();
    let messages = CatalogFixture::demo(11).inbound_messages();

    {
        let core = syncing_core_at(&db, 1, &remote);
        core.init_device(InitDeviceCommand {
            tenant_id: TenantId::new("TENANT-DEMO").unwrap(),
            store_id: StoreId::new("STORE-DEMO").unwrap(),
            currency: Currency::Mxn,
            allow_negative_inventory: true,
        })
        .unwrap();

        // With nothing to send, a cycle commits the empty claim, then the
        // cursor read, then the import's own transaction — and the fourth
        // commit is the cursor write. That is the one to kill.
        remote.script_inbound([messages[0].clone()]);
        core.store().arm_commit_failure_after(3);
        assert!(core.run_sync_cycle().is_err());

        // The effect is durable; the cursor is not. That is the safe order:
        // the message will come again.
        assert!(core
            .get_catalog_status()
            .unwrap()
            .importing
            .iter()
            .any(|view| view.snapshot.version() == 11));
        assert_eq!(core.get_sync_status().unwrap().inbound_cursor, None);
    }

    // The server, never acknowledged, sends the whole sequence again.
    let core = syncing_core_at(&db, 10_000, &remote);
    remote.script_inbound(messages);
    for _ in 0..4 {
        core.run_sync_cycle().unwrap();
    }

    // One snapshot, in force, and the cursor now reflects it.
    let status = core.get_catalog_status().unwrap();
    assert_eq!(
        status.active.map(|view| view.snapshot.version()),
        Some(11),
        "the redelivered begin resumed the open import instead of opening a second one"
    );
    assert_eq!(
        core.get_sync_status().unwrap().inbound_cursor,
        Some("catalog-11-3".to_owned())
    );
}

#[test]
fn an_unanswered_batch_is_retried_and_lands_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let remote = transport();
    let core = syncing_core_at(&db, 1, &remote);
    open_register(&core);
    a_finalized_sale(&core);

    // Received, then silence. PI.Edge cannot know, so it asks again.
    remote.script_outbound([
        SimulatorBehavior::ReceiveThenTimeout { elapsed_ms: 15_000 },
        SimulatorBehavior::ReceiveThenTimeout { elapsed_ms: 15_000 },
    ]);
    for _ in 0..12 {
        core.run_sync_cycle().unwrap();
        if core.get_sync_status().unwrap().outbox.sent == 1 {
            break;
        }
    }

    let id = remote.deliveries()[0];
    assert_eq!(
        remote.delivery_count(id),
        3,
        "two unanswered, then answered"
    );
    assert_eq!(remote.distinct_facts_received(), 1);
    assert_eq!(core.get_sync_status().unwrap().outbox.sent, 1);

    // The sale is exactly as it was. Sync never touches commercial state.
    let reopened = SqliteEdgeStore::open(&db).unwrap();
    drop(reopened);
    assert_eq!(core.get_edge_state().unwrap().pending_outbox, 0);
}
