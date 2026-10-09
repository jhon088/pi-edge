//! The payment surface over the real IPC transport.
//!
//! What only this layer can show: the operation names a UI calls, the stable
//! codes it branches on, and that a daemon killed mid-payment comes back with
//! a financial state that is safe to act on.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use edge_client::EdgeClient;
use edge_protocol::{
    ChangeQuantityRequestV1, PaymentRequestV1, RemoveItemRequestV1, StartPaymentRequestV1,
    TransactionRequestV1,
};

/// The demo sale: 100.00 at 16% = 116.00.
const TOTAL: i64 = 11_600;

fn cash(amount_minor: i64) -> StartPaymentRequestV1 {
    StartPaymentRequestV1 {
        tender_type: "CASH".to_owned(),
        amount_minor,
    }
}

fn card(amount_minor: i64) -> StartPaymentRequestV1 {
    StartPaymentRequestV1 {
        tender_type: "CARD".to_owned(),
        amount_minor,
    }
}

fn sale_ready(client: &edge_client::IpcEdgeClient) {
    open_register(client);
    client.start_transaction(None).unwrap();
    client.add_item(demo_item(), None).unwrap();
    client.totalize(None).unwrap();
}

#[test]
fn a_mixed_tender_sale_settles_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    sale_ready(&client);

    let partial = client.start_payment(cash(3_000), None).unwrap();
    assert_eq!(partial.payment.tender_type, "CASH");
    assert_eq!(partial.payment.status, "COMMITTED");
    assert!(partial.payment.attempts.is_empty(), "cash has no attempts");
    assert_eq!(partial.settlement.remaining_balance.minor, 8_600);
    assert!(!partial.settlement.can_finalize);
    assert!(partial.settlement.basket_locked);

    let completed = client.start_payment(card(8_600), None).unwrap();
    assert_eq!(completed.payment.tender_type, "CARD");
    assert_eq!(
        completed.payment.provider_kind.as_deref(),
        Some("SIMULATOR")
    );
    assert_eq!(completed.payment.status, "COMMITTED");
    assert_eq!(completed.payment.attempts.len(), 1);
    assert_eq!(completed.payment.attempts[0].status, "AUTHORIZED");
    assert!(completed.payment.attempts[0].provider_reference.is_some());
    assert!(!completed.payment.attempts[0].unresolved);
    assert_eq!(completed.settlement.remaining_balance.minor, 0);
    assert!(completed.settlement.can_finalize);
    assert_eq!(completed.transaction.status, "COMMITTED");

    // Both payments are listed for the sale, newest last.
    let listing = client
        .get_transaction_payments(TransactionRequestV1 {
            transaction_id: completed.transaction.transaction_id.clone(),
        })
        .unwrap();
    assert_eq!(listing.payments.len(), 2);
    assert_eq!(listing.settlement.committed_total.minor, TOTAL);

    // One payment can be fetched on its own.
    let fetched = client
        .get_payment(PaymentRequestV1 {
            payment_id: completed.payment.payment_id.clone(),
        })
        .unwrap();
    assert_eq!(fetched.payment, completed.payment);

    let finalized = client.finalize_transaction(None).unwrap();
    assert_eq!(finalized.status, "FINALIZED");
}

#[test]
fn cash_overpayment_reports_the_change_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    sale_ready(&client);

    let paid = client.start_payment(cash(20_000), None).unwrap();
    assert_eq!(paid.payment.requested_amount.formatted, "200.00");
    assert_eq!(paid.payment.committed_amount.formatted, "116.00");
    assert_eq!(paid.payment.change_due.formatted, "84.00");
    assert_eq!(paid.settlement.tendered_total.formatted, "200.00");
    assert_eq!(paid.settlement.committed_total.formatted, "116.00");
    assert_eq!(paid.transaction.change_due.formatted, "84.00");
}

#[test]
fn a_declined_card_surfaces_a_stable_code_and_leaves_the_sale_payable() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_with_payment_script(&database, Some("default:decline"));
    let client = client(&database, "tauri-pos");
    sale_ready(&client);

    let declined = client.start_payment(card(TOTAL), None).unwrap();
    // A refusal is a recorded outcome, not a transport error: the payment
    // exists, failed, and explains itself.
    assert_eq!(declined.payment.status, "FAILED");
    assert_eq!(declined.payment.attempts[0].status, "FAILED");
    assert_eq!(
        declined.payment.attempts[0].failure_code.as_deref(),
        Some("SIMULATED_DECLINED")
    );
    assert_eq!(declined.settlement.remaining_balance.minor, TOTAL);
    assert!(!declined.settlement.basket_locked);

    // The sale is payable again by another tender.
    let paid = client.start_payment(cash(TOTAL), None).unwrap();
    assert!(paid.settlement.can_finalize);
}

#[test]
fn an_unknown_card_blocks_the_sale_until_it_is_reconciled() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_with_payment_script(
        &database,
        Some("default:approve,amount:11600=unknown,query:approve"),
    );
    let client = client(&database, "tauri-pos");
    sale_ready(&client);

    let unknown = client.start_payment(card(TOTAL), None).unwrap();
    assert_eq!(unknown.payment.attempts[0].status, "UNKNOWN");
    assert!(unknown.payment.attempts[0].unresolved);
    assert!(unknown.settlement.requires_reconciliation);
    assert!(!unknown.settlement.can_finalize);

    // A second equivalent charge is refused: it could duplicate the first.
    let blocked = client
        .start_payment(card(TOTAL), None)
        .expect_err("must not risk charging twice");
    assert_eq!(blocked.code(), "PAYMENT_REQUIRES_RECONCILIATION");

    // So is finalizing, and so is editing the basket.
    assert_eq!(
        client
            .finalize_transaction(None)
            .expect_err("the money state is unknown")
            .code(),
        "INVALID_TRANSACTION_STATE"
    );
    assert_eq!(
        client
            .remove_item(RemoveItemRequestV1 { line_number: 1 }, None)
            .expect_err("the basket is frozen")
            .code(),
        "BASKET_LOCKED_BY_PAYMENT"
    );
    assert_eq!(
        client
            .change_quantity(
                ChangeQuantityRequestV1 {
                    line_number: 1,
                    quantity_thousandths: 2_000,
                },
                None,
            )
            .expect_err("the basket is frozen")
            .code(),
        "BASKET_LOCKED_BY_PAYMENT"
    );

    // Reconciling asks the provider and resolves the same interaction.
    let resolved = client
        .reconcile_payment(
            PaymentRequestV1 {
                payment_id: unknown.payment.payment_id.clone(),
            },
            None,
        )
        .unwrap();
    assert_eq!(resolved.payment.attempts.len(), 1, "no new charge");
    assert_eq!(resolved.payment.attempts[0].status, "AUTHORIZED");
    assert_eq!(resolved.payment.status, "COMMITTED");
    assert!(resolved.settlement.can_finalize);
    assert_eq!(
        client.finalize_transaction(None).unwrap().status,
        "FINALIZED"
    );
}

#[test]
fn a_reversal_returns_the_sale_to_payable_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    sale_ready(&client);

    let committed = client.start_payment(card(TOTAL), None).unwrap();
    let reversed = client
        .reverse_payment(
            PaymentRequestV1 {
                payment_id: committed.payment.payment_id.clone(),
            },
            None,
        )
        .unwrap();
    assert_eq!(reversed.payment.status, "REVERSED");
    assert_eq!(reversed.payment.attempts[0].status, "REVERSED");
    assert_eq!(reversed.payment.committed_amount.minor, 0);
    assert_eq!(reversed.settlement.remaining_balance.minor, TOTAL);
    assert!(!reversed.settlement.basket_locked);
    assert_eq!(reversed.transaction.status, "TOTALIZED");

    // Reversing again is refused rather than silently repeated.
    assert_eq!(
        client
            .reverse_payment(
                PaymentRequestV1 {
                    payment_id: committed.payment.payment_id.clone(),
                },
                None,
            )
            .expect_err("already reversed")
            .code(),
        "PAYMENT_REVERSAL_NOT_ALLOWED"
    );
}

#[test]
fn an_unknown_payment_id_is_a_stable_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    sale_ready(&client);

    assert_eq!(
        client
            .get_payment(PaymentRequestV1 {
                payment_id: "00000000-0000-0000-0000-0000000000ff".to_owned(),
            })
            .expect_err("no such payment")
            .code(),
        "PAYMENT_NOT_FOUND"
    );
    assert_eq!(
        client
            .get_payment(PaymentRequestV1 {
                payment_id: "not-a-uuid".to_owned(),
            })
            .expect_err("not an id at all")
            .code(),
        "PAYMENT_INVALID_RECORD"
    );
}

/// The crash case the durable ordering exists for: the daemon dies while a
/// card payment is in flight, and the next runtime must recover something
/// safe to act on — never "it failed".
#[test]
fn a_daemon_killed_mid_payment_recovers_a_reconcilable_state() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let payment_id;
    {
        let mut daemon = DaemonProcess::start_with_payment_script(
            &database,
            Some("default:approve,amount:11600=unknown"),
        );
        let client = client(&database, "edge-cli");
        sale_ready(&client);
        let unknown = client.start_payment(card(TOTAL), None).unwrap();
        assert_eq!(unknown.payment.attempts[0].status, "UNKNOWN");
        payment_id = unknown.payment.payment_id.clone();
        daemon.kill_hard();
    }

    // A brand new runtime over the same database, now able to query.
    let _daemon = DaemonProcess::start_with_payment_script(&database, Some("query:approve"));
    let client = client(&database, "edge-cli");
    let recovered = client
        .get_payment(PaymentRequestV1 {
            payment_id: payment_id.clone(),
        })
        .unwrap();
    assert_eq!(
        recovered.payment.attempts[0].status, "UNKNOWN",
        "a restart must not decide the outcome"
    );
    assert!(!recovered.settlement.can_finalize);
    assert_eq!(
        client
            .finalize_transaction(None)
            .expect_err("still unresolved")
            .code(),
        "INVALID_TRANSACTION_STATE"
    );

    let resolved = client
        .reconcile_payment(PaymentRequestV1 { payment_id }, None)
        .unwrap();
    assert_eq!(resolved.payment.status, "COMMITTED");
    assert_eq!(resolved.settlement.remaining_balance.minor, 0);
    assert_eq!(
        client.finalize_transaction(None).unwrap().status,
        "FINALIZED"
    );
}

#[test]
fn the_payment_journal_is_written_once_per_transition() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    sale_ready(&client);
    let paid = client.start_payment(card(TOTAL), None).unwrap();
    client
        .reverse_payment(
            PaymentRequestV1 {
                payment_id: paid.payment.payment_id.clone(),
            },
            None,
        )
        .unwrap();

    let events: Vec<String> = client
        .get_journal(100)
        .unwrap()
        .into_iter()
        .rev()
        .map(|event| event.event_type)
        .filter(|name| name.starts_with("PAYMENT_"))
        .collect();
    assert_eq!(
        events,
        vec![
            "PAYMENT_STARTED",
            "PAYMENT_COMMITTED",
            "PAYMENT_REVERSAL_REQUESTED",
            "PAYMENT_REVERSED",
        ]
    );

    // Payments produce no outbox traffic of their own: the sale is what
    // upstream cares about, and it has not been finalized yet (ADR 020).
    assert_eq!(client.get_pending_outbox(10).unwrap().pending_count, 0);
}

#[test]
fn a_replayed_start_payment_over_ipc_charges_once() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    sale_ready(&client);

    let request_id = uuid::Uuid::from_u128(0xBEEF);
    let first = client.start_payment(card(5_000), Some(request_id)).unwrap();
    let replay = client.start_payment(card(5_000), Some(request_id)).unwrap();
    assert_eq!(first.payment.payment_id, replay.payment.payment_id);

    let listing = client
        .get_transaction_payments(TransactionRequestV1 {
            transaction_id: first.transaction.transaction_id.clone(),
        })
        .unwrap();
    assert_eq!(listing.payments.len(), 1);
    assert_eq!(listing.payments[0].attempts.len(), 1);
    assert_eq!(listing.settlement.committed_total.minor, 5_000);
}

/// The ordering guarantee of ADR 020, reproduced rather than simulated.
///
/// The provider is scripted to approve and then kill the process *before the
/// result can be persisted*. So: the attempt is durable, the customer is
/// charged, and the daemon dies knowing nothing about the outcome. What comes
/// back must be unresolved — never failed, never a second charge.
#[test]
fn a_crash_between_the_provider_call_and_the_result_leaves_a_reconcilable_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let transaction_id;
    {
        let mut daemon = DaemonProcess::start_with_payment_script(
            &database,
            Some("default:approve,crash:after-answer"),
        );
        let client = client(&database, "edge-cli");
        sale_ready(&client);
        transaction_id = client
            .get_active_transaction()
            .unwrap()
            .unwrap()
            .transaction_id;

        // The provider approves and the runtime dies on the spot, so the call
        // cannot answer.
        let dropped = client
            .start_payment(card(TOTAL), None)
            .expect_err("the runtime died mid-payment");
        assert_eq!(dropped.code(), "CORE_UNAVAILABLE");
        assert!(
            daemon.wait_until_dead(std::time::Duration::from_secs(5)),
            "the provider hook must have aborted the runtime"
        );
        daemon.kill_hard();
    }

    // --- a new runtime over the same database -----------------------------
    let _daemon = DaemonProcess::start_with_payment_script(&database, Some("query:approve"));
    let client = client(&database, "edge-cli");
    let listing = client
        .get_transaction_payments(TransactionRequestV1 {
            transaction_id: transaction_id.clone(),
        })
        .unwrap();

    // The attempt exists: phase 1 made it durable before the call.
    assert_eq!(listing.payments.len(), 1, "exactly one payment, not zero");
    let payment = &listing.payments[0];
    assert_eq!(payment.attempts.len(), 1, "and exactly one attempt");
    let attempt = &payment.attempts[0];

    // It is unresolved, and specifically NOT failed: we asked and we do not
    // know, which is the truth.
    assert_eq!(attempt.status, "PROCESSING");
    assert!(attempt.unresolved);
    assert_ne!(attempt.status, "FAILED");
    assert_eq!(payment.status, "IN_PROGRESS");
    assert_eq!(payment.committed_amount.minor, 0);
    assert!(listing.settlement.requires_reconciliation);
    assert!(!listing.settlement.can_finalize);
    assert!(listing.settlement.basket_locked);

    // Nothing was retried automatically, and nothing may be: another charge
    // could duplicate the one we cannot see.
    assert_eq!(
        client
            .start_payment(card(TOTAL), None)
            .expect_err("must not risk a double charge")
            .code(),
        "PAYMENT_REQUIRES_RECONCILIATION"
    );
    assert_eq!(
        client
            .finalize_transaction(None)
            .expect_err("the money state is unknown")
            .code(),
        "INVALID_TRANSACTION_STATE"
    );

    // The only way forward is a query, and it resolves the same interaction.
    let resolved = client
        .reconcile_payment(
            PaymentRequestV1 {
                payment_id: payment.payment_id.clone(),
            },
            None,
        )
        .unwrap();
    assert_eq!(resolved.payment.attempts.len(), 1, "no new attempt");
    assert_eq!(
        resolved.payment.attempts[0].correlation_id, attempt.correlation_id,
        "the same correlation identity was reconciled"
    );
    assert_eq!(resolved.payment.attempts[0].status, "AUTHORIZED");
    assert_eq!(resolved.payment.status, "COMMITTED");
    assert_eq!(resolved.settlement.remaining_balance.minor, 0);
    assert_eq!(
        client.finalize_transaction(None).unwrap().status,
        "FINALIZED"
    );
}
