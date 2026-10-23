//! PI.Edge as a complete POS over the real transport, with no server.
//!
//! What only this layer shows: that the whole standalone closure is reachable
//! through the Edge Protocol by a separate process, that the receipt comes out
//! because the **runtime** decided to, and that a restart resolves what the
//! previous one left in the air (ADR 043).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use edge_client::EdgeClient;

const SKU: &str = "SKU-001";
const UNIT: i64 = 11_600;

/// A standalone till: explicit simulators, no sync, development refund
/// authority. Every one of these is named out loud (ADR 038, ADR 040).
const STANDALONE: [(&str, &str); 3] = [
    ("PI_EDGE_PRINTER", "simulator"),
    ("PI_EDGE_DRAWER", "simulator"),
    ("PI_EDGE_SYNC", "none"),
];

#[test]
fn a_till_sells_prints_refunds_and_closes_with_no_server_configured() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_with_env(&database, None, true, &STANDALONE);
    let client = client(&database, "edge-cli");

    // Nothing to sync to, and that is a healthy register.
    assert_eq!(client.get_sync_status().unwrap().driver, "unconfigured");

    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&client, UNIT);

    // The receipt came out because the runtime closed the sale, not because a
    // client asked (ADR 043).
    let receipt = client.get_receipt(&sale).unwrap();
    assert_eq!(receipt.status, "PRINTED");
    assert_eq!(receipt.attempt_count, 1);
    assert!(receipt.first_printed_at.is_some());

    // The operational history knows it, with its own materialised total.
    let history = client.list_recent_sales(10, 0).unwrap();
    assert_eq!(history.sales.len(), 1);
    assert_eq!(history.sales[0].transaction_id, sale);
    assert_eq!(history.sales[0].grand_total.minor, UNIT);
    assert_eq!(history.sales[0].receipt_status.as_deref(), Some("PRINTED"));
    assert_eq!(history.attention.pending, 0);
    assert_eq!(history.attention.failed, 0);
    assert_eq!(history.attention.unknown, 0);

    // A refund, with the development authority this installation was started
    // with — the production rule is unchanged (ADR 042).
    let refund = client
        .create_refund(
            edge_protocol::CreateRefundRequestV1 {
                transaction_id: sale.clone(),
                lines: vec![edge_protocol::RefundLineRequestV1 {
                    original_line_number: 1,
                    quantity_thousandths: 1_000,
                }],
                reason_code: "CUSTOMER_RETURN".to_owned(),
                fiscal_action: "NONE".to_owned(),
            },
            None,
        )
        .unwrap();
    let refund_id = refund.refund_id.clone();
    let request = edge_protocol::RefundRequestV1 {
        refund_id: refund_id.clone(),
    };
    client.accept_refund(request.clone(), None).unwrap();
    let paid = client.start_refund_payment(request, None).unwrap();
    assert_eq!(paid.status, "COMPLETED");
    assert_eq!(inventory_position(&client, SKU).on_hand, "10.000");

    // Close the till and the day. The receipt is settled, so nothing is
    // flagged.
    client.request_session_close(None).unwrap();
    let closed = client
        .declare_closing_cash(
            edge_protocol::DeclareClosingCashRequestV1 {
                counted_minor: 100_000,
            },
            None,
        )
        .unwrap();
    assert_eq!(closed.variance_label, "BALANCED");
    assert_eq!(closed.receipt_attention.pending, 0);
    assert_eq!(closed.receipt_attention.unknown, 0);
    // The day close answers the day *and* what still needs somebody, so a
    // client never has to go looking for it (ADR 043).
    let day = client.close_business_day(None).unwrap();
    assert_eq!(day.day.status, "CLOSED");
    assert_eq!(day.receipt_attention.pending, 0);
    assert_eq!(day.receipt_attention.failed, 0);
    assert_eq!(day.receipt_attention.unknown, 0);

    // Everything that happened is on record, waiting for a server that does
    // not exist.
    let cycle = client.run_sync_cycle(None).unwrap();
    assert_eq!(cycle.outcome, "NOT_CONFIGURED");
    assert!(client.get_sync_status().unwrap().pending >= 4);
}

#[test]
fn a_till_with_no_printer_closes_its_sales_and_says_what_is_pending() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    // No PI_EDGE_PRINTER at all: the production default is "not configured",
    // never a simulator (ADR 038).
    let _daemon = DaemonProcess::start_with_env(&database, None, true, &[("PI_EDGE_SYNC", "none")]);
    let client = client(&database, "edge-cli");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);

    let sale = finalized_cash_sale(&client, UNIT);
    // The sale is closed and its stock and cash moved.
    assert_eq!(inventory_position(&client, SKU).on_hand, "9.000");
    assert_eq!(client.cash_position().unwrap().cash_sales.minor, UNIT);

    // Nothing was sent to any printer, so the obligation is retryable and
    // visible — not unknown.
    let receipt = client.get_receipt(&sale).unwrap();
    assert_eq!(receipt.status, "FAILED");
    assert_eq!(
        receipt.last_error_code.as_deref(),
        Some("HARDWARE_NOT_CONFIGURED")
    );
    assert_eq!(client.list_recent_sales(10, 0).unwrap().attention.failed, 1);

    // And the day closes, reporting it rather than refusing.
    client.request_session_close(None).unwrap();
    let closed = client
        .declare_closing_cash(
            edge_protocol::DeclareClosingCashRequestV1 {
                counted_minor: 100_000 + UNIT,
            },
            None,
        )
        .unwrap();
    assert_eq!(closed.variance_label, "BALANCED");
    assert_eq!(closed.receipt_attention.failed, 1);
    // And the day closes, reporting the same thing in its own result.
    let day = client.close_business_day(None).unwrap();
    assert_eq!(day.day.status, "CLOSED");
    assert_eq!(day.receipt_attention.failed, 1);
    assert_eq!(day.receipt_attention.unknown, 0);
}

#[test]
fn an_abandoned_sale_does_not_wedge_the_till() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_with_env(&database, None, true, &STANDALONE);
    let client = client(&database, "edge-cli");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);

    client.start_transaction(None).unwrap();
    client.add_item(demo_item(), None).unwrap();
    let cancelled = client
        .cancel_transaction(Some("CUSTOMER_LEFT".to_owned()), None)
        .unwrap();
    assert_eq!(cancelled.status, "CANCELLED");

    // Nothing happened, so there is nothing to report, print or close around.
    let state = client.get_edge_state().unwrap();
    assert!(state.active_transaction.is_none());
    assert_eq!(state.pending_outbox, 0);
    assert_eq!(client.list_recent_sales(10, 0).unwrap().sales.len(), 0);
    assert_eq!(
        client
            .get_receipt(&cancelled.transaction_id)
            .unwrap_err()
            .code(),
        "RECEIPT_NOT_FOUND"
    );

    client.request_session_close(None).unwrap();
    client
        .declare_closing_cash(
            edge_protocol::DeclareClosingCashRequestV1 {
                counted_minor: 100_000,
            },
            None,
        )
        .unwrap();
    client.close_business_day(None).unwrap();
}
