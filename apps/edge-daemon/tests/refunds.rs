//! Refunds over the real IPC transport.
//!
//! What only this layer can show: that **authority over the original sale is
//! resolved by the runtime and refused by default** (ADR 030), that a client
//! cannot assert it or declare a fiscal state, and that the refundable figures
//! a UI would render are the core's own.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use edge_client::EdgeClient;
use edge_protocol::{
    CreateRefundRequestV1, RefundLineRequestV1, RefundRequestV1, TransactionRequestV1,
};

const SKU: &str = "SKU-001";
const UNIT: i64 = 11_600;

fn create_request(transaction_id: &str, units: i64, fiscal: &str) -> CreateRefundRequestV1 {
    CreateRefundRequestV1 {
        transaction_id: transaction_id.to_owned(),
        lines: vec![RefundLineRequestV1 {
            original_line_number: 1,
            quantity_thousandths: units * 1_000,
        }],
        reason_code: "CUSTOMER_RETURN".to_owned(),
        fiscal_action: fiscal.to_owned(),
    }
}

/// The production default: no authority source configured, so no refund.
#[test]
fn without_a_configured_authority_every_refund_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_configured(&database, None, false);
    let client = client(&database, "edge-cli");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&client, UNIT);

    let error = client
        .create_refund(create_request(&sale, 1, "NONE"), None)
        .unwrap_err();

    // A locally finalized sale is not a centrally accepted one, and nothing in
    // the payload can make it one.
    assert_eq!(error.code(), "REFUND_AUTHORITY_REQUIRED");
    // Eligibility still answers: reading what *could* be refunded needs no
    // authority, only refunding does.
    let view = client
        .refund_eligibility(TransactionRequestV1 {
            transaction_id: sale.clone(),
        })
        .unwrap();
    assert_eq!(view.refundable_total.formatted, "116.00");
    assert!(view.refund_ids.is_empty());
}

#[test]
fn the_eligibility_view_carries_every_figure_a_ui_would_render() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&client, 40_000);

    let view = client
        .refund_eligibility(TransactionRequestV1 {
            transaction_id: sale.clone(),
        })
        .unwrap();

    assert_eq!(view.transaction_id, sale);
    assert_eq!(view.original_status, "FINALIZED");
    assert_eq!(view.fiscal_state, "NOT_INVOICED");
    assert!(view.invoice_ref.is_none());
    assert_eq!(view.lines.len(), 1);
    let line = &view.lines[0];
    // Quantities arrive rendered, like every quantity on this wire.
    assert_eq!(line.sold_quantity, "1.000");
    assert_eq!(line.refunded_quantity, "0.000");
    assert_eq!(line.remaining_quantity, "1.000");
    assert_eq!(line.remaining_gross.formatted, "116.00");
    assert_eq!(line.remaining_net.formatted, "100.00");
    assert_eq!(line.remaining_tax.formatted, "16.00");
    // The tender ceiling is what that tender applied, not what was handed over.
    assert_eq!(view.tenders.len(), 1);
    assert_eq!(view.tenders[0].tender_type, "CASH");
    assert_eq!(view.tenders[0].committed.formatted, "116.00");
    assert_eq!(view.tenders[0].remaining.formatted, "116.00");
}

#[test]
fn a_whole_refund_crosses_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&client, UNIT);

    let created = client
        .create_refund(create_request(&sale, 1, "NONE"), None)
        .unwrap();
    assert_eq!(created.status, "CREATED");
    assert_eq!(created.authority, "CENTRAL_ACCEPTED");
    // The development stand-in marks what it produced, so development data is
    // never mistaken for a real authorisation.
    assert!(created
        .authorization_ref
        .as_deref()
        .is_some_and(|reference| reference.starts_with("DEV-LOCAL:")));
    assert_eq!(created.gross.formatted, "116.00");
    assert!(!created.inventory_returned);
    assert_eq!(created.fiscal_status, "NONE");

    let accepted = client
        .accept_refund(
            RefundRequestV1 {
                refund_id: created.refund_id.clone(),
            },
            None,
        )
        .unwrap();
    assert_eq!(accepted.status, "ACCEPTED");
    assert!(accepted.inventory_returned);
    assert_eq!(inventory_position(&client, SKU).on_hand, "10.000");

    let paid = client
        .start_refund_payment(
            RefundRequestV1 {
                refund_id: created.refund_id.clone(),
            },
            None,
        )
        .unwrap();
    assert_eq!(paid.status, "COMPLETED");
    assert_eq!(paid.remaining_to_refund.formatted, "0.00");
    assert_eq!(paid.payments.len(), 1);
    assert_eq!(paid.payments[0].tender_type, "CASH");
    assert_eq!(paid.payments[0].status, "REFUNDED");
    // Cash has no provider, so there is no attempt history to show.
    assert!(paid.payments[0].attempts.is_empty());
    assert!(!paid.requires_reconciliation);

    let drawer = client.cash_position().unwrap();
    assert_eq!(drawer.cash_refunds.formatted, "116.00");

    let history = client
        .transaction_refunds(TransactionRequestV1 {
            transaction_id: sale,
        })
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].refund_id, created.refund_id);
}

#[test]
fn a_card_refund_whose_answer_is_lost_comes_back_unresolved() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_with_payment_script(
        &database,
        Some("default:approve,refund:unknown,refund-query:approve"),
    );
    let client = client(&database, "edge-cli");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);

    client.start_transaction(None).unwrap();
    client.add_item(demo_item(), None).unwrap();
    client.totalize(None).unwrap();
    client
        .start_payment(
            edge_protocol::StartPaymentRequestV1 {
                tender_type: "CARD".to_owned(),
                amount_minor: UNIT,
            },
            None,
        )
        .unwrap();
    let sale = client.finalize_transaction(None).unwrap().transaction_id;

    let created = client
        .create_refund(create_request(&sale, 1, "NONE"), None)
        .unwrap();
    let request = RefundRequestV1 {
        refund_id: created.refund_id.clone(),
    };
    client.accept_refund(request.clone(), None).unwrap();
    let pending = client.start_refund_payment(request.clone(), None).unwrap();

    // Unresolved, not failed, and the goods stay returned (ADR 033).
    assert_eq!(pending.status, "FINANCIAL_PENDING");
    assert!(pending.requires_reconciliation);
    assert!(pending.inventory_returned);
    assert_eq!(pending.payments[0].status, "PENDING");
    assert_eq!(pending.payments[0].attempts.len(), 1);
    assert_eq!(pending.payments[0].attempts[0].status, "UNKNOWN");
    let correlation = pending.payments[0].attempts[0].correlation_id.clone();

    // Starting again refuses rather than sending a second refund.
    let refused = client
        .start_refund_payment(request.clone(), None)
        .unwrap_err();
    assert_eq!(refused.code(), "REFUND_REQUIRES_RECONCILIATION");

    let resolved = client.reconcile_refund(request, None).unwrap();
    assert_eq!(resolved.status, "COMPLETED");
    assert_eq!(resolved.payments[0].status, "REFUNDED");
    // The same attempt resolved: never a second one.
    assert_eq!(resolved.payments[0].attempts.len(), 1);
    assert_eq!(resolved.payments[0].attempts[0].correlation_id, correlation);
    assert!(!resolved.requires_reconciliation);
}

/// The fiscal frontier as a till actually meets it.
///
/// Nothing on this wire can declare a sale invoiced — PI.Edge consumes fiscal
/// metadata and is not the authority for writing it (ADR 032) — so every sale
/// a till can reach is `NOT_INVOICED`, and asking for a credit note for an
/// invoice that does not exist is refused. The invoiced branch is arranged by
/// a fixture and pinned in the integration and golden suites.
#[test]
fn a_till_cannot_invoice_a_sale_and_cannot_ask_for_a_credit_note() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&client, UNIT);

    // The sale carries its fiscal metadata, written when it finalized, and it
    // says what is true: nobody invoiced it.
    let view = client
        .refund_eligibility(TransactionRequestV1 {
            transaction_id: sale.clone(),
        })
        .unwrap();
    assert_eq!(view.fiscal_state, "NOT_INVOICED");
    assert!(view.invoice_ref.is_none());

    // And there is no operation that could change that: the protocol has no
    // message for it at all.
    for fiscal in ["CREDIT_NOTE_PENDING", "CREDIT_NOTE_REQUESTED"] {
        let refused = client
            .create_refund(create_request(&sale, 1, fiscal), None)
            .unwrap_err();
        assert_eq!(refused.code(), "REFUND_FISCAL_ACTION_INVALID", "{fiscal}");
    }

    // `NONE` is the only truthful answer, and it is recorded explicitly.
    let created = client
        .create_refund(create_request(&sale, 1, "NONE"), None)
        .unwrap();
    assert_eq!(created.fiscal_status, "NONE");
    assert_eq!(created.original_fiscal_state, "NOT_INVOICED");
}

#[test]
fn a_malformed_refund_payload_is_invalid_argument() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&client, UNIT);

    // A non-positive quantity is a malformed request, the same as anywhere
    // else on this wire.
    let mut bad = create_request(&sale, 1, "NONE");
    bad.lines[0].quantity_thousandths = 0;
    assert_eq!(
        client.create_refund(bad, None).unwrap_err().code(),
        "INVALID_ARGUMENT"
    );

    // A fiscal action the contract does not define is equally malformed. That
    // a *defined* action is wrong for this sale is a different answer
    // (`REFUND_FISCAL_ACTION_INVALID`), pinned in its own test.
    let unknown_action = create_request(&sale, 1, "CREDIT_NOTE_COMPLETED");
    assert_eq!(
        client
            .create_refund(unknown_action, None)
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );

    assert_eq!(
        client
            .get_refund(RefundRequestV1 {
                refund_id: "not-a-uuid".to_owned()
            })
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
}

#[test]
fn the_cli_drives_a_refund_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&client, 40_000);

    let eligibility = run_cli(&database, &["refund-eligibility", "--transaction", &sale]);
    assert_eq!(eligibility["refundable_total"]["formatted"], "116.00");

    let created = run_cli(
        &database,
        &[
            "refund-create",
            "--transaction",
            &sale,
            "--line",
            "1:1000",
            "--reason",
            "CUSTOMER_RETURN",
        ],
    );
    let refund_id = created["refund_id"].as_str().unwrap().to_owned();
    assert_eq!(created["gross"]["formatted"], "116.00");

    run_cli(&database, &["refund-accept", "--refund", &refund_id]);
    let paid = run_cli(&database, &["refund-pay", "--refund", &refund_id]);
    assert_eq!(paid["status"], "COMPLETED");

    // Two processes, one contract, one refund.
    let shown = run_cli(&database, &["refund-show", "--refund", &refund_id]);
    assert_eq!(shown["refund_id"], refund_id.as_str());
    assert_eq!(inventory_position(&client, SKU).on_hand, "10.000");
    assert_eq!(
        client.cash_position().unwrap().cash_refunds.formatted,
        "116.00"
    );
}

/// Run the CLI against a daemon and parse its stdout.
fn run_cli(database: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = std::process::Command::new(cli_binary())
        .arg("--db")
        .arg(database)
        .arg("--format")
        .arg("json")
        .args(args)
        .env("PI_EDGE_LOG", "warn")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "edge-cli {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The crash that matters: the provider answers the **refund** and the process
/// dies before the result can be persisted. So the attempt is durable, the
/// money may already be on its way back, and the daemon dies knowing nothing.
/// What comes back must be unresolved — never failed, and never a second
/// refund (ADR 020, ADR 034).
///
/// The charge hook is left disarmed so the sale settles normally: the window
/// under test is the refund's, not the sale's.
#[test]
fn a_crash_between_the_refund_call_and_the_result_leaves_a_reconcilable_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let refund_id;
    {
        let mut daemon = DaemonProcess::start_with_payment_script(
            &database,
            Some("default:approve,refund-crash:after-answer"),
        );
        let client = client(&database, "edge-cli");
        open_register(&client);
        bootstrap_stock(&client, &[(SKU, 10)]);

        // A card sale, so the refund has a provider-backed tender to go back
        // through.
        client.start_transaction(None).unwrap();
        client.add_item(demo_item(), None).unwrap();
        client.totalize(None).unwrap();
        client
            .start_payment(
                edge_protocol::StartPaymentRequestV1 {
                    tender_type: "CARD".to_owned(),
                    amount_minor: UNIT,
                },
                None,
            )
            .unwrap();
        let sale = client.finalize_transaction(None).unwrap().transaction_id;

        let created = client
            .create_refund(create_request(&sale, 1, "NONE"), None)
            .unwrap();
        refund_id = created.refund_id.clone();
        client
            .accept_refund(
                RefundRequestV1 {
                    refund_id: refund_id.clone(),
                },
                None,
            )
            .unwrap();

        // The provider refunds and the runtime dies on the spot.
        let dropped = client
            .start_refund_payment(
                RefundRequestV1 {
                    refund_id: refund_id.clone(),
                },
                None,
            )
            .expect_err("the runtime died mid-refund");
        assert_eq!(dropped.code(), "CORE_UNAVAILABLE");
        assert!(
            daemon.wait_until_dead(std::time::Duration::from_secs(5)),
            "the provider hook must have aborted the runtime"
        );
        daemon.kill_hard();
    }

    // --- a new runtime over the same database -----------------------------
    let _daemon = DaemonProcess::start_with_payment_script(&database, Some("refund-query:approve"));
    let client = client(&database, "edge-cli");
    let request = RefundRequestV1 {
        refund_id: refund_id.clone(),
    };
    let view = client.get_refund(request.clone()).unwrap();

    // The attempt exists: phase 1 made it durable before the call.
    assert_eq!(view.payments.len(), 1, "exactly one refund payment");
    assert_eq!(
        view.payments[0].attempts.len(),
        1,
        "and exactly one attempt"
    );
    let attempt = &view.payments[0].attempts[0];
    // Unresolved, and specifically not failed: we asked and we do not know.
    assert_eq!(attempt.status, "PROCESSING");
    assert_eq!(view.payments[0].status, "PENDING");
    assert_eq!(view.status, "FINANCIAL_PENDING");
    assert!(view.requires_reconciliation);
    assert_eq!(view.payments[0].refunded.formatted, "0.00");
    let correlation = attempt.correlation_id.clone();

    // The goods stayed returned through the crash: the commercial refund does
    // not un-happen because the card is in doubt (ADR 033).
    assert!(view.inventory_returned);
    assert_eq!(inventory_position(&client, SKU).on_hand, "10.000");

    // Starting again refuses: no second refund, ever, on a guess.
    assert_eq!(
        client
            .start_refund_payment(request.clone(), None)
            .unwrap_err()
            .code(),
        "REFUND_REQUIRES_RECONCILIATION"
    );

    // The way out is asking about that same attempt.
    let resolved = client.reconcile_refund(request, None).unwrap();
    assert_eq!(resolved.status, "COMPLETED");
    assert_eq!(resolved.payments[0].status, "REFUNDED");
    assert_eq!(resolved.payments[0].attempts.len(), 1);
    assert_eq!(resolved.payments[0].attempts[0].correlation_id, correlation);
    assert_eq!(resolved.payments[0].refunded.formatted, "116.00");
}
