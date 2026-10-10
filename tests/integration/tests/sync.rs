//! The sync engine against real SQLite, and no server anywhere.
//!
//! The policy itself — batch formation, ordering, backoff, outcome
//! classification — is pinned by unit tests in `edge-sync`. What only the whole
//! runtime shows is here: that a till with nothing configured sells
//! indefinitely and keeps every fact, that a restart loses nothing, that a
//! fact in flight when the process died comes back with the same id, that a
//! batch the server received but never acknowledged ends up delivered once,
//! and that an inbound message's effect is durable before its cursor moves.
//!
//! No HTTP, no URL, no fake server. The simulator accepts batches and answers
//! them; it implements none of PI.COMMERCE Server's business rules (ADR 040).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::queries::SyncCycleOutcome;
use edge_application::testing::CatalogFixture;
use edge_domain::prelude::*;
use edge_integration_tests::*;
use edge_payments::{SimulatedOutcome, SimulatorScript};
use edge_refunds::{FiscalState, RefundFiscalStatus, RefundStatus};
use edge_sync::simulator::{SimulatorBehavior, SIMULATOR};
use edge_sync::unconfigured::UNCONFIGURED;
use edge_sync::{InboundKind, InboundMessage};
use uuid::Uuid;

/// Sell one demo item for cash and finish the sale.
fn a_sale(core: &TestCore) {
    cash_sale(core, 11_600);
}

/// Close the till and the day: two more facts.
fn close_the_day(core: &TestCore) {
    core.request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    core.declare_closing_cash(DeclareClosingCashCommand {
        request_id: None,
        counted_minor: 111_600,
    })
    .unwrap();
    core.close_business_day(CloseBusinessDayCommand::default())
        .unwrap();
}

/// Run cycles until nothing more moves, bounded so a bug cannot hang the suite.
fn drain(fixture: &SyncFixture) {
    for _ in 0..12 {
        let report = fixture.core.run_sync_cycle().unwrap();
        if report.attempted == 0 && report.inbound_applied == 0 {
            return;
        }
    }
}

fn drain_scripted(fixture: &ScriptedSyncFixture) {
    for _ in 0..12 {
        let report = fixture.core.run_sync_cycle().unwrap();
        if report.attempted == 0 && report.inbound_applied == 0 {
            return;
        }
    }
}

/// A card sale of one demo unit, settled on the card so a refund has a
/// provider-backed tender to give back.
fn card_sale_for_refund(core: &ScriptedCore) -> TransactionId {
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: Tender::Card,
        amount_minor: 11_600,
    })
    .unwrap();
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap()
        .id()
}

/// `(aggregate_id, aggregate_version)` for the facts in a delivered batch.
fn versions_of(fixture: &SyncFixture, batch: &[Uuid]) -> Vec<(String, i64)> {
    let mut all = Vec::new();
    for status in [
        OutboxStatus::Pending,
        OutboxStatus::Sent,
        OutboxStatus::Failed,
    ] {
        all.extend(outbox_entries(&fixture.core, status));
    }
    batch
        .iter()
        .filter_map(|id| {
            all.iter()
                .find(|entry| entry.event.event_id.to_uuid() == *id)
                .map(|entry| {
                    (
                        entry.event.aggregate_id.clone(),
                        entry.event.aggregate_version,
                    )
                })
        })
        .collect()
}

// ------------------------------------------------- a register with no server

#[test]
fn a_register_with_no_sync_configured_sells_and_says_so_plainly() {
    let core = in_memory_core();
    open_register(&core);
    a_sale(&core);

    let status = core.get_sync_status().unwrap();
    // Not a simulator. Absence selects nothing (ADR 040).
    assert_eq!(status.driver, UNCONFIGURED);
    assert_eq!(status.outbox.pending, 1);
    assert_eq!(status.inbound_cursor, None);

    // Asking to sync is answered, not refused: there is simply nowhere to
    // send, and the fact stays exactly where it was.
    let report = core.run_sync_cycle().unwrap();
    assert_eq!(report.outcome, SyncCycleOutcome::NotConfigured);
    assert_eq!(report.acknowledged, 0);
    assert_eq!(core.get_sync_status().unwrap().outbox.pending, 1);
}

#[test]
fn a_whole_day_offline_keeps_every_fact_it_produced() {
    let core = in_memory_core();
    open_register(&core);
    a_sale(&core);
    a_sale(&core);
    a_sale(&core);
    close_the_day(&core);

    // Three sales, the session close and the day close. Nothing delivered,
    // nothing lost, and the day closed without ever asking a server.
    let status = core.get_sync_status().unwrap();
    assert_eq!(status.outbox.pending, 5);
    assert_eq!(status.outbox.sent, 0);
    assert_eq!(status.outbox.failed, 0);
}

#[test]
fn several_days_offline_accumulate_in_order_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi-edge.db");

    {
        let core = offline_core_at(&path);
        open_register(&core);
        a_sale(&core);
        close_the_day(&core);
    }
    // A new process, two days later, on the same installation.
    {
        let core = core_two_days_later(&path);
        core.open_business_day(OpenBusinessDayCommand::default())
            .unwrap();
        core.open_register_session(OpenRegisterSessionCommand {
            request_id: None,
            register_id: RegisterId::new("REG-001").unwrap(),
            operator_id: OperatorId::new("CASHIER-001").unwrap(),
            opening_cash_minor: 100_000,
        })
        .unwrap();
        a_sale(&core);
        close_the_day(&core);

        let status = core.get_sync_status().unwrap();
        assert_eq!(status.outbox.pending, 6, "two days of facts, all of them");
        // `seq` is the installation's durable order and survives the restart.
        let pending = outbox_entries(&core, OutboxStatus::Pending);
        let seqs: Vec<i64> = pending.iter().map(|entry| entry.seq).collect();
        let mut sorted = seqs.clone();
        sorted.sort_unstable();
        assert_eq!(seqs, sorted);
        assert_eq!(
            pending
                .iter()
                .map(|entry| entry.event.event_type.as_str())
                .collect::<Vec<_>>(),
            vec![
                "SALE_FINALIZED",
                "REGISTER_SESSION_CLOSED",
                "BUSINESS_DAY_CLOSED",
                "SALE_FINALIZED",
                "REGISTER_SESSION_CLOSED",
                "BUSINESS_DAY_CLOSED",
            ]
        );
    }
}

#[test]
fn a_restart_hands_the_pending_facts_to_the_next_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi-edge.db");
    let before: Vec<EventId>;
    {
        let core = offline_core_at(&path);
        open_register(&core);
        a_sale(&core);
        a_sale(&core);
        before = outbox_ids(&core, OutboxStatus::Pending);
        assert_eq!(before.len(), 2);
    }
    let core = core_two_days_later(&path);
    assert_eq!(
        outbox_ids(&core, OutboxStatus::Pending),
        before,
        "the same facts, with the same ids the server will deduplicate on"
    );
}

// --------------------------------------------------------------- dispatching

#[test]
fn an_explicitly_configured_simulator_receives_the_batch_and_acknowledges_it() {
    let fixture = SyncFixture::in_memory();
    assert_eq!(fixture.core.sync().driver(), SIMULATOR);
    open_register(&fixture.core);
    a_sale(&fixture.core);
    close_the_day(&fixture.core);

    let report = fixture.core.run_sync_cycle().unwrap();
    assert_eq!(report.outcome, SyncCycleOutcome::Progressed);
    assert_eq!(report.attempted, 3);
    assert_eq!(report.acknowledged, 3);
    assert_eq!(report.failed, 0);

    let status = fixture.core.get_sync_status().unwrap();
    assert_eq!(status.outbox.pending, 0);
    assert_eq!(status.outbox.sending, 0);
    assert_eq!(status.outbox.sent, 3);

    // Delivered once each, and the batch was one transport call.
    assert_eq!(fixture.transport.distinct_facts_received(), 3);
    assert_eq!(fixture.transport.batches().len(), 1);

    // Nothing is purged: the delivered facts are still here (ADR 041).
    assert_eq!(outbox_entries(&fixture.core, OutboxStatus::Sent).len(), 3);
}

#[test]
fn facts_travel_oldest_first_and_a_batch_is_capped() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    for _ in 0..5 {
        a_sale(&fixture.core);
    }
    let expected = outbox_ids(&fixture.core, OutboxStatus::Pending);

    // Two at a time, so the ordering across batches is visible.
    let fixture = fixture.with_batch_limit(2);
    drain(&fixture);

    assert_eq!(
        fixture.transport.deliveries(),
        expected.iter().map(|id| id.to_uuid()).collect::<Vec<_>>(),
        "every fact exactly once, in `seq` order"
    );
    let sizes: Vec<usize> = fixture.transport.batches().iter().map(Vec::len).collect();
    assert_eq!(sizes, vec![2, 2, 1], "the limit held");
}

#[test]
fn a_transient_failure_returns_the_fact_to_pending_with_a_later_attempt() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    a_sale(&fixture.core);
    fixture
        .transport
        .script_outbound([SimulatorBehavior::TransientAll {
            reason: "the server is busy".to_owned(),
        }]);

    let report = fixture.core.run_sync_cycle().unwrap();
    assert_eq!(report.attempted, 1);
    assert_eq!(report.rescheduled, 1);
    assert_eq!(report.acknowledged, 0);

    let pending = outbox_entries(&fixture.core, OutboxStatus::Pending);
    assert_eq!(pending.len(), 1);
    let entry = &pending[0];
    assert_eq!(entry.event.attempt_count, 1, "the attempt was counted");
    assert!(
        entry.event.next_attempt_at.is_some(),
        "a retry is scheduled, not immediate"
    );
    assert_eq!(
        entry.last_error_code.as_deref(),
        Some("SYNC_TRANSIENT_FAILURE"),
        "the reason is kept where an operator can read it"
    );

    // And it does go, next time.
    drain(&fixture);
    assert_eq!(fixture.core.get_sync_status().unwrap().outbox.sent, 1);
}

#[test]
fn a_permanent_rejection_is_terminal_visible_and_never_retried() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    a_sale(&fixture.core);
    fixture
        .transport
        .script_outbound([SimulatorBehavior::PermanentAll {
            code: "CONTRACT_REJECTED".to_owned(),
            reason: "this fact type is not accepted".to_owned(),
        }]);

    let report = fixture.core.run_sync_cycle().unwrap();
    assert_eq!(report.failed, 1);

    let status = fixture.core.get_sync_status().unwrap();
    assert_eq!(status.outbox.failed, 1);
    assert_eq!(status.outbox.pending, 0);
    let visible = &status.failed[0];
    assert_eq!(visible.error_code.as_deref(), Some("CONTRACT_REJECTED"));
    assert_eq!(visible.event_type, OutboxEventType::SaleFinalized);

    // Nothing retries it and nothing deletes it: it waits for a person.
    let delivered = fixture.transport.distinct_facts_received();
    drain(&fixture);
    assert_eq!(fixture.transport.distinct_facts_received(), delivered);
    assert_eq!(fixture.core.get_sync_status().unwrap().outbox.failed, 1);
    assert_eq!(outbox_entries(&fixture.core, OutboxStatus::Failed).len(), 1);
}

#[test]
fn a_terminal_fact_does_not_hold_back_unrelated_facts() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    a_sale(&fixture.core);
    fixture
        .transport
        .script_outbound([SimulatorBehavior::PermanentAll {
            code: "CONTRACT_REJECTED".to_owned(),
            reason: "no".to_owned(),
        }]);
    fixture.core.run_sync_cycle().unwrap();

    // A second, unrelated sale still syncs.
    a_sale(&fixture.core);
    drain(&fixture);
    let status = fixture.core.get_sync_status().unwrap();
    assert_eq!(status.outbox.failed, 1);
    assert_eq!(status.outbox.sent, 1);
    assert_eq!(status.outbox.pending, 0);
}

#[test]
fn an_unreachable_server_changes_nothing_and_is_not_an_error() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    a_sale(&fixture.core);
    fixture
        .transport
        .script_outbound([SimulatorBehavior::Unavailable {
            detail: "no route to host".to_owned(),
        }]);

    let report = fixture.core.run_sync_cycle().unwrap();
    match report.outcome {
        SyncCycleOutcome::TransportFailed { code, .. } => {
            assert_eq!(code, "SYNC_UNAVAILABLE");
        }
        other => panic!("expected a transport failure, got {other:?}"),
    }
    assert_eq!(fixture.transport.distinct_facts_received(), 0);
    assert_eq!(fixture.core.get_sync_status().unwrap().outbox.pending, 1);

    // The till kept selling throughout.
    a_sale(&fixture.core);
    assert_eq!(fixture.core.get_sync_status().unwrap().outbox.pending, 2);
}

#[test]
fn an_unreadable_answer_is_not_a_partial_success() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    a_sale(&fixture.core);
    a_sale(&fixture.core);
    fixture
        .transport
        .script_outbound([SimulatorBehavior::MalformedReceipt]);

    let report = fixture.core.run_sync_cycle().unwrap();
    match report.outcome {
        SyncCycleOutcome::TransportFailed { code, .. } => {
            assert_eq!(code, "SYNC_MALFORMED_RESPONSE");
        }
        other => panic!("expected a malformed answer, got {other:?}"),
    }
    // Neither fact was concluded from an answer we could not read.
    let status = fixture.core.get_sync_status().unwrap();
    assert_eq!(status.outbox.pending, 2);
    assert_eq!(status.outbox.sent, 0);
    assert_eq!(status.outbox.failed, 0);
}

// ------------------------------------------------------ crash in mid-dispatch

#[test]
fn a_fact_in_flight_when_the_runtime_died_is_queued_again_with_the_same_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi-edge.db");
    let claimed: Vec<EventId>;
    {
        let core = offline_core_at(&path);
        open_register(&core);
        a_sale(&core);
        // The claim committed; the process died before any answer arrived.
        claimed = claim_and_abandon(&core, &outbox_ids(&core, OutboxStatus::Pending));
        assert_eq!(claimed.len(), 1);
        assert_eq!(core.get_sync_status().unwrap().outbox.sending, 1);
    }

    // The next runtime does not leave it stuck. No lease, no heartbeat: this
    // database has one writer, so `SENDING` after a restart means nobody is
    // coming back for it (ADR 041).
    let fixture = SyncFixture::reopen(&path, 0, 10_000);
    assert_eq!(fixture.core.recover_interrupted_sync().unwrap(), 1);
    let status = fixture.core.get_sync_status().unwrap();
    assert_eq!(status.outbox.sending, 0);
    assert_eq!(status.outbox.pending, 1);

    // The same event id, which is what makes the resend safe.
    assert_eq!(outbox_ids(&fixture.core, OutboxStatus::Pending), claimed);
    drain(&fixture);
    assert_eq!(fixture.transport.deliveries(), vec![claimed[0].to_uuid()]);
    assert_eq!(fixture.core.get_sync_status().unwrap().outbox.sent, 1);
}

#[test]
fn a_batch_the_server_received_before_the_crash_is_delivered_twice_and_counted_once() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    a_sale(&fixture.core);
    let id = outbox_ids(&fixture.core, OutboxStatus::Pending)[0];

    // The worst case: the server has the fact, PI.Edge never learns it.
    fixture
        .transport
        .script_outbound([SimulatorBehavior::ReceiveThenTimeout { elapsed_ms: 30_000 }]);
    let report = fixture.core.run_sync_cycle().unwrap();
    match report.outcome {
        SyncCycleOutcome::TransportFailed { code, .. } => assert_eq!(code, "SYNC_TIMEOUT"),
        other => panic!("expected a timeout, got {other:?}"),
    }
    assert_eq!(fixture.transport.delivery_count(id.to_uuid()), 1);
    assert_eq!(fixture.core.get_sync_status().unwrap().outbox.pending, 1);

    // The replay carries the same id, so the second delivery is a duplicate
    // the consumer deduplicates — at-least-once, deliberately (ADR 004).
    drain(&fixture);
    assert_eq!(fixture.transport.delivery_count(id.to_uuid()), 2);
    assert_eq!(
        fixture.transport.distinct_facts_received(),
        1,
        "one logical fact, however many deliveries"
    );
    assert_eq!(fixture.core.get_sync_status().unwrap().outbox.sent, 1);

    // And the sale itself is untouched by any of it.
    assert_eq!(on_hand(&fixture.core, "SKU-001"), -1_000);
}

#[test]
fn a_second_acknowledgement_moves_nothing() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    a_sale(&fixture.core);
    drain(&fixture);
    let delivered = fixture.transport.deliveries();
    assert_eq!(delivered.len(), 1);

    // A delivered fact is not dispatchable, so a further cycle is idle and
    // the server is not told twice.
    let report = fixture.core.run_sync_cycle().unwrap();
    assert_eq!(report.outcome, SyncCycleOutcome::Idle);
    assert_eq!(report.attempted, 0);
    assert_eq!(fixture.transport.deliveries(), delivered);
    assert_eq!(fixture.core.get_sync_status().unwrap().outbox.sent, 1);
}

// ------------------------------------------------------------ what travels out

#[test]
fn a_sale_is_published_exactly_once_however_many_cycles_run() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    a_sale(&fixture.core);
    for _ in 0..5 {
        fixture.core.run_sync_cycle().unwrap();
    }
    assert_eq!(fixture.transport.deliveries().len(), 1);
}

#[test]
fn a_refund_is_published_twice_and_its_restock_is_not() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    bootstrap_stock(&fixture.core, &[("SKU-001", 10)]);
    let sale = finalized_cash_sale(&fixture.core, demo_item(), 11_600);
    let refund = full_refund(&fixture.core, sale, refund_lines(&[(1, 1)]));
    assert!(refund.refund.completed_at.is_some());

    // Two facts about one refund: the commercial return, then the money. And
    // no separate movement fact for the stock that came back, because it is
    // derivable from the refund (ADR 028, ADR 041).
    assert_eq!(
        fixture.fact_types(),
        vec!["SALE_FINALIZED", "REFUND_ACCEPTED", "REFUND_COMPLETED"]
    );
    drain(&fixture);
    assert_eq!(fixture.transport.distinct_facts_received(), 3);

    // Exactly once each, however many cycles.
    for _ in 0..3 {
        fixture.core.run_sync_cycle().unwrap();
    }
    assert_eq!(fixture.transport.deliveries().len(), 3);
}

/// The commercial return does not wait for the money.
///
/// A card refund whose answer was lost leaves the refund in
/// `FINANCIAL_PENDING`, possibly for days. `REFUND_ACCEPTED` is already
/// durable; `REFUND_COMPLETED` does not exist yet and appears only when the
/// reconciliation resolves the attempt (ADR 041).
#[test]
fn an_accepted_refund_is_published_even_while_the_money_is_pending() {
    let fixture =
        SyncFixture::scripted(SimulatorScript::approving().refunding(SimulatedOutcome::Unknown));
    open_scripted_register(&fixture.core);
    let sale = card_sale_for_refund(&fixture.core);

    let refund = fixture
        .core
        .create_refund(CreateRefundCommand {
            request_id: None,
            authorization: central_authority(sale),
            lines: refund_lines(&[(1, 1)]),
            reason_code: "CUSTOMER_RETURN".to_owned(),
            fiscal_action: RefundFiscalStatus::None,
        })
        .unwrap();
    let refund_id = refund.refund.refund_id;

    // CREATED: nothing published. A refund nobody has handed the goods back
    // for is not yet a fact about the business.
    assert!(
        fixture.refund_facts().is_empty(),
        "creating a refund publishes nothing: the goods are not back yet"
    );

    fixture
        .core
        .accept_refund(AcceptRefundCommand {
            request_id: None,
            refund_id,
        })
        .unwrap();
    assert_eq!(
        fixture.refund_facts(),
        vec![("REFUND_ACCEPTED".to_owned(), 1)],
        "the irrevocable commercial return is published at acceptance"
    );

    // The money goes out and the provider's answer is lost.
    let paid = fixture
        .core
        .start_refund_payment(StartRefundPaymentCommand {
            request_id: None,
            refund_id,
        })
        .unwrap();
    assert_eq!(paid.refund.status, RefundStatus::FinancialPending);
    assert!(paid.requires_reconciliation);
    assert_eq!(
        fixture.refund_facts(),
        vec![("REFUND_ACCEPTED".to_owned(), 1)],
        "still no REFUND_COMPLETED: the obligation is not settled"
    );

    // The return is already on its way to the consumer while the money is
    // unresolved — which is the whole point.
    drain_scripted(&fixture);
    assert_eq!(fixture.transport.distinct_facts_received(), 2);

    // Reconciliation resolves the attempt, and only now does version 2 exist.
    let settled = fixture
        .core
        .reconcile_refund(ReconcileRefundCommand {
            request_id: None,
            refund_id,
        })
        .unwrap();
    assert_eq!(settled.refund.status, RefundStatus::Completed);
    assert_eq!(
        fixture.refund_facts(),
        vec![
            ("REFUND_ACCEPTED".to_owned(), 1),
            ("REFUND_COMPLETED".to_owned(), 2),
        ]
    );
    drain_scripted(&fixture);
    assert_eq!(fixture.transport.distinct_facts_received(), 3);
}

// ----------------------------------------- two versions of one aggregate

/// The ordering guarantee, on a real aggregate that really has two versions.
///
/// `REFUND_ACCEPTED` is version 1 and `REFUND_COMPLETED` is version 2 of the
/// same refund. Version 2 is never in flight before version 1 is acknowledged,
/// and that is enforced by `seq` plus one fact per aggregate per batch — no
/// dependency graph (ADR 041).
#[test]
fn a_later_version_of_a_refund_waits_until_the_earlier_one_is_acknowledged() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    bootstrap_stock(&fixture.core, &[("SKU-001", 10)]);
    let sale = finalized_cash_sale(&fixture.core, demo_item(), 11_600);
    // A second, unrelated sale, to show it is not held back by any of this.
    let other = finalized_cash_sale(&fixture.core, demo_item(), 11_600);
    assert_ne!(sale, other);
    // Accept and pay with no cycle in between, so both refund facts are
    // pending at once.
    let refund = full_refund(&fixture.core, sale, refund_lines(&[(1, 1)]));
    let refund_id = refund.refund.refund_id.to_string();

    // (1) both versions pending, on the same aggregate.
    let pending = outbox_entries(&fixture.core, OutboxStatus::Pending);
    let refund_facts: Vec<(&str, i64)> = pending
        .iter()
        .filter(|entry| entry.event.aggregate_id == refund_id)
        .map(|entry| {
            (
                entry.event.event_type.as_str(),
                entry.event.aggregate_version,
            )
        })
        .collect();
    assert_eq!(
        refund_facts,
        vec![("REFUND_ACCEPTED", 1), ("REFUND_COMPLETED", 2)]
    );
    // (6) one aggregate id for both.
    assert_eq!(
        pending
            .iter()
            .filter(|entry| entry.event.aggregate_id == refund_id)
            .count(),
        2
    );

    // (2,3) the first batch takes version 1 and leaves version 2 behind —
    // while both unrelated sales travel in the same batch.
    fixture
        .transport
        .script_outbound([SimulatorBehavior::TransientFor {
            event_ids: Vec::new(),
            reason: String::new(),
        }]);
    let report = fixture.core.run_sync_cycle().unwrap();
    assert_eq!(report.attempted, 3, "two sales plus one refund fact");
    let first_batch = &fixture.transport.batches()[0];
    let sent_versions = versions_of(&fixture, first_batch);
    assert_eq!(
        sent_versions
            .iter()
            .filter(|(aggregate, _)| aggregate == &refund_id)
            .map(|(_, version)| *version)
            .collect::<Vec<_>>(),
        vec![1],
        "only version 1 of the refund was in flight"
    );
    // (4,5) version 1 is acknowledged, so the next cycle may send version 2.
    assert_eq!(report.acknowledged, 3);
    let report = fixture.core.run_sync_cycle().unwrap();
    assert_eq!(report.attempted, 1);
    assert_eq!(report.acknowledged, 1);

    // (7) both versions delivered, in order.
    let delivered = fixture.transport.deliveries();
    assert_eq!(delivered.len(), 4);
    assert_eq!(
        fixture.core.get_sync_status().unwrap().outbox.sent,
        4,
        "two sales and two refund facts"
    );

    // (8) a replay of version 1 is one logical fact however often it arrives.
    let accepted = outbox_entries(&fixture.core, OutboxStatus::Sent)
        .into_iter()
        .find(|entry| entry.event.event_type == OutboxEventType::RefundAccepted)
        .expect("REFUND_ACCEPTED was delivered");
    assert_eq!(
        fixture
            .transport
            .delivery_count(accepted.event.event_id.to_uuid()),
        1
    );
    for _ in 0..3 {
        fixture.core.run_sync_cycle().unwrap();
    }
    assert_eq!(
        fixture.transport.distinct_facts_received(),
        4,
        "no further cycle re-sends a delivered fact"
    );
}

/// A permanently rejected version 1 holds back version 2 of *that* refund, and
/// nothing else.
#[test]
fn a_rejected_refund_acceptance_blocks_its_completion_and_nothing_else() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    bootstrap_stock(&fixture.core, &[("SKU-001", 10)]);
    let sale = finalized_cash_sale(&fixture.core, demo_item(), 11_600);
    drain(&fixture);
    let refund = full_refund(&fixture.core, sale, refund_lines(&[(1, 1)]));
    let refund_id = refund.refund.refund_id.to_string();

    // (9) version 1 is rejected for good.
    fixture
        .transport
        .script_outbound([SimulatorBehavior::PermanentAll {
            code: "CONTRACT_REJECTED".to_owned(),
            reason: "this consumer does not accept refunds yet".to_owned(),
        }]);
    let report = fixture.core.run_sync_cycle().unwrap();
    assert_eq!(report.failed, 1);

    // Version 2 is never sent: handing the consumer a completion whose
    // acceptance it rejected would give it a history with a hole in it.
    drain(&fixture);
    let status = fixture.core.get_sync_status().unwrap();
    assert_eq!(status.outbox.failed, 1);
    assert_eq!(status.outbox.pending, 1, "the completion still waits");
    assert_eq!(status.failed[0].aggregate_id, refund_id);
    assert_eq!(status.failed[0].event_type, OutboxEventType::RefundAccepted);

    // (10) another sale, on another aggregate, syncs regardless.
    let before = fixture.transport.distinct_facts_received();
    finalized_cash_sale(&fixture.core, demo_item(), 11_600);
    drain(&fixture);
    assert_eq!(fixture.transport.distinct_facts_received(), before + 1);
    assert_eq!(
        fixture.core.get_sync_status().unwrap().outbox.pending,
        1,
        "still only the blocked completion"
    );
}

#[test]
fn a_manual_stock_movement_is_published_and_a_sale_movement_is_not() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    bootstrap_stock(&fixture.core, &[("SKU-001", 10)]);
    fixture
        .core
        .inventory_in(InventoryInCommand {
            request_id: None,
            product_ref: ProductRef::new("SKU-001").unwrap(),
            quantity: Quantity::from_units(2).unwrap(),
            reason_code: "RECEIPT".to_owned(),
        })
        .unwrap();
    a_sale(&fixture.core);

    // The bootstrap is not a fact about this installation's trading — it came
    // from upstream — and the sale's depletion rides on `SALE_FINALIZED`.
    assert_eq!(
        fixture.fact_types(),
        vec!["INVENTORY_MOVEMENT_RECORDED", "SALE_FINALIZED"]
    );
    drain(&fixture);
    assert_eq!(fixture.core.get_sync_status().unwrap().outbox.sent, 2);
}

#[test]
fn the_session_and_the_day_are_published_when_they_close() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    a_sale(&fixture.core);
    close_the_day(&fixture.core);
    assert_eq!(
        fixture.fact_types(),
        vec![
            "SALE_FINALIZED",
            "REGISTER_SESSION_CLOSED",
            "BUSINESS_DAY_CLOSED"
        ]
    );
}

// ------------------------------------------------------------ what comes in

#[test]
fn a_catalog_arrives_through_the_existing_import_lifecycle() {
    let fixture = SyncFixture::in_memory();
    init_device(&fixture.core);
    assert!(fixture.core.get_catalog_status().unwrap().active.is_none());

    fixture
        .transport
        .script_inbound(CatalogFixture::demo(7).inbound_messages());
    let report = fixture.core.run_sync_cycle().unwrap();
    assert_eq!(report.inbound_applied, 4);

    // In force, and usable: the sale path does not know a catalog arrived by
    // sync rather than by CLI.
    let status = fixture.core.get_catalog_status().unwrap();
    assert_eq!(status.active.map(|view| view.snapshot.version()), Some(7));
    open_day_and_session(&fixture.core);
    a_sale(&fixture.core);

    // The cursor is where the last message said.
    assert_eq!(
        fixture.core.get_sync_status().unwrap().inbound_cursor,
        Some("catalog-7-3".to_owned())
    );
}

#[test]
fn prices_and_promotions_arrive_with_the_catalog_and_price_the_sale() {
    let fixture = SyncFixture::in_memory();
    init_device(&fixture.core);
    // One inbound contract: products, price books, prices and promotions are
    // one snapshot, not three channels (ADR 041).
    fixture
        .transport
        .script_inbound(promotional_catalog(3).inbound_messages());
    drain(&fixture);

    open_day_and_session(&fixture.core);
    fixture
        .core
        .start_transaction(StartTransactionCommand::default())
        .unwrap();
    fixture.core.add_item(scan("7501000000001", 3)).unwrap();
    let sale = fixture
        .core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    // Buy 2 get 1, applied by the same evaluator as always: three units
    // priced as two.
    assert_eq!(sale.state().totals.grand_total.minor(), 23_200);
    assert!(!sale.state().applied_promotions.is_empty());
}

#[test]
fn a_redelivered_catalog_message_changes_nothing() {
    let fixture = SyncFixture::in_memory();
    init_device(&fixture.core);
    let messages = CatalogFixture::demo(4).inbound_messages();
    fixture.transport.script_inbound(messages.clone());
    drain(&fixture);
    let active = fixture.core.get_catalog_status().unwrap().active;

    // The server, having lost our acknowledgement, sends the lot again.
    fixture.transport.script_inbound(messages);
    drain(&fixture);
    assert_eq!(
        fixture
            .core
            .get_catalog_status()
            .unwrap()
            .active
            .map(|view| view.snapshot.id()),
        active.map(|view| view.snapshot.id()),
        "the same snapshot, not a second one"
    );
}

#[test]
fn an_unreadable_inbound_message_stops_the_cursor_where_it_was() {
    let fixture = SyncFixture::in_memory();
    init_device(&fixture.core);
    let mut messages = CatalogFixture::demo(5).inbound_messages();
    // The chunk arrives with a payload this contract cannot read.
    messages[1].payload = serde_json::json!({ "version": 5, "nonsense": true });
    fixture.transport.script_inbound(messages);

    let report = fixture.core.run_sync_cycle().unwrap();
    match report.outcome {
        SyncCycleOutcome::TransportFailed { code, .. } => {
            assert_eq!(code, "SYNC_MALFORMED_RESPONSE");
        }
        other => panic!("expected a malformed message, got {other:?}"),
    }
    assert_eq!(report.inbound_applied, 1, "only the message that applied");

    // The import opened and stayed open. Nothing is partially in force, and
    // the cursor sits before the message that failed, so it will be
    // redelivered rather than skipped (ADR 041).
    let status = fixture.core.get_catalog_status().unwrap();
    assert!(status.active.is_none(), "nothing partially active");
    assert_eq!(
        fixture.core.get_sync_status().unwrap().inbound_cursor,
        Some("catalog-5-0".to_owned())
    );
}

#[test]
fn a_chunk_for_an_import_that_was_never_opened_is_refused() {
    let fixture = SyncFixture::in_memory();
    init_device(&fixture.core);
    // The begin message is missing, so the chunk names a version this
    // installation has never heard of.
    let messages = CatalogFixture::demo(6).inbound_messages();
    fixture.transport.script_inbound([messages[1].clone()]);

    let report = fixture.core.run_sync_cycle().unwrap();
    assert_eq!(report.inbound_applied, 0);
    assert_eq!(fixture.core.get_sync_status().unwrap().inbound_cursor, None);
}

#[test]
fn fiscal_metadata_only_ever_arrives_from_outside() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    let sale = finalized_cash_sale(&fixture.core, demo_item(), 11_600);

    // PI.Edge wrote `NOT_INVOICED` at finalization and has no way of its own
    // to change it: no operation, no protocol message, no CLI command
    // (ADR 032).
    let eligibility = fixture.core.get_refund_eligibility(sale).unwrap();
    assert_eq!(eligibility.fiscal_state, FiscalState::NotInvoiced);

    fixture.transport.script_inbound([InboundMessage {
        message_id: Uuid::from_u128(4_711),
        cursor: "fiscal-1".to_owned(),
        kind: InboundKind::FiscalMetadata,
        payload: serde_json::json!({
            "transaction_id": sale.to_string(),
            "fiscal_state": "INVOICED",
            "invoice_ref": "A-2026-0001",
        }),
    }]);
    let report = fixture.core.run_sync_cycle().unwrap();
    assert_eq!(report.inbound_applied, 1);

    let eligibility = fixture.core.get_refund_eligibility(sale).unwrap();
    assert_eq!(eligibility.fiscal_state, FiscalState::Invoiced);
    assert_eq!(
        fixture.core.get_sync_status().unwrap().inbound_cursor,
        Some("fiscal-1".to_owned())
    );
}

#[test]
fn an_invoiced_sale_cannot_be_un_invoiced_by_a_later_message() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    let sale = finalized_cash_sale(&fixture.core, demo_item(), 11_600);
    let invoiced = |cursor: &str, state: &str, reference: serde_json::Value| InboundMessage {
        message_id: Uuid::new_v4(),
        cursor: cursor.to_owned(),
        kind: InboundKind::FiscalMetadata,
        payload: serde_json::json!({
            "transaction_id": sale.to_string(),
            "fiscal_state": state,
            "invoice_ref": reference,
        }),
    };
    fixture.transport.script_inbound([
        invoiced("f-1", "INVOICED", serde_json::json!("A-2026-0002")),
        invoiced("f-2", "NOT_INVOICED", serde_json::Value::Null),
    ]);
    drain(&fixture);

    // Fiscal state moves forward only. A re-sent `NOT_INVOICED` is a no-op,
    // never a rewrite of a sale that was invoiced (ADR 032).
    assert_eq!(
        fixture
            .core
            .get_refund_eligibility(sale)
            .unwrap()
            .fiscal_state,
        FiscalState::Invoiced
    );
}

#[test]
fn an_invoiced_message_with_no_reference_is_refused() {
    let fixture = SyncFixture::in_memory();
    open_register(&fixture.core);
    let sale = finalized_cash_sale(&fixture.core, demo_item(), 11_600);
    fixture.transport.script_inbound([InboundMessage {
        message_id: Uuid::from_u128(4_712),
        cursor: "fiscal-bad".to_owned(),
        kind: InboundKind::FiscalMetadata,
        payload: serde_json::json!({
            "transaction_id": sale.to_string(),
            "fiscal_state": "INVOICED",
            "invoice_ref": "   ",
        }),
    }]);
    let report = fixture.core.run_sync_cycle().unwrap();
    assert_eq!(report.inbound_applied, 0);
    assert_eq!(
        fixture
            .core
            .get_refund_eligibility(sale)
            .unwrap()
            .fiscal_state,
        FiscalState::NotInvoiced
    );
    assert_eq!(fixture.core.get_sync_status().unwrap().inbound_cursor, None);
}

#[test]
fn a_commercial_setting_arrives_inbound_and_hardware_never_does() {
    let fixture = SyncFixture::in_memory();
    init_device(&fixture.core);
    assert!(fixture.core.get_device().unwrap().allow_negative_inventory);

    fixture.transport.script_inbound([InboundMessage {
        message_id: Uuid::from_u128(5_000),
        cursor: "settings-1".to_owned(),
        kind: InboundKind::StoreSettings,
        // One field, because one field has a consumer. A printer host or a
        // drawer driver is local to the composition root and is not in this
        // contract (ADR 039, ADR 040).
        payload: serde_json::json!({ "allow_negative_inventory": false }),
    }]);
    drain(&fixture);

    let device = fixture.core.get_device().unwrap();
    assert!(!device.allow_negative_inventory);
    // Identity is provisioned, not synced: the message could not have changed
    // it even if it had tried.
    assert_eq!(device.store_id.as_str(), "STORE-DEMO");
    assert_eq!(device.currency, Currency::Mxn);
}

#[test]
fn an_unknown_inbound_kind_cannot_be_expressed() {
    // The inbound kinds are a closed set, so a server that invents a message
    // produces a parse failure at the edge rather than something to skip.
    assert!(InboundKind::parse("SOMETHING_NEW").is_err());
    assert!(InboundKind::parse("FISCAL_METADATA").is_ok());
}
