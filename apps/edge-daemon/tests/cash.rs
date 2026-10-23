//! Cash management over the real IPC transport.
//!
//! The one thing only this layer can prove: the blind close is enforced by the
//! **contract**. A cashier's close path receives a payload that has no field
//! capable of carrying the expected cash, and no amount in it anywhere
//! (ADR 022).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use edge_client::EdgeClient;
use edge_protocol::{
    ApproveSafeDropRequestV1, CashMovementRequestV1, DeclareClosingCashRequestV1,
    ForceCloseBusinessDayRequestV1, ReopenSessionRequestV1, StartPaymentRequestV1,
};

const FLOAT: i64 = 100_000;
const SALE: i64 = 11_600;

fn open_till(client: &edge_client::IpcEdgeClient) {
    open_register(client);
}

fn cash_sale(client: &edge_client::IpcEdgeClient, tendered_minor: i64) {
    client.start_transaction(None).unwrap();
    client.add_item(demo_item(), None).unwrap();
    client.totalize(None).unwrap();
    client
        .start_payment(
            StartPaymentRequestV1 {
                tender_type: "CASH".to_owned(),
                amount_minor: tendered_minor,
            },
            None,
        )
        .unwrap();
    client.finalize_transaction(None).unwrap();
}

#[test]
fn the_cash_position_reports_the_drawer_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    open_till(&client);

    // 200.00 tendered for a 116.00 sale: the drawer nets 116.00.
    cash_sale(&client, 20_000);
    let position = client.cash_position().unwrap();
    assert_eq!(position.opening_float.formatted, "1000.00");
    assert_eq!(position.cash_sales.formatted, "116.00");
    assert_eq!(position.expected_cash.formatted, "1116.00");
    assert_eq!(position.tendered_total.formatted, "200.00");
    assert_eq!(position.change_total.formatted, "84.00");
    assert_eq!(position.movements.len(), 2);
    let sale = &position.movements[1];
    assert_eq!(sale.movement_type, "CASH_SALE");
    assert_eq!(sale.direction, "IN");
    assert_eq!(sale.amount.formatted, "116.00");
    assert_eq!(sale.tendered.as_ref().unwrap().formatted, "200.00");
    assert_eq!(sale.change.as_ref().unwrap().formatted, "84.00");
}

#[test]
fn cash_in_and_out_move_the_drawer_and_demand_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    open_till(&client);

    let after_in = client
        .cash_in(
            CashMovementRequestV1 {
                amount_minor: 50_000,
                reason_code: "CHANGE_FUND".to_owned(),
            },
            None,
        )
        .unwrap();
    assert_eq!(after_in.expected_cash.formatted, "1500.00");

    let after_out = client
        .cash_out(
            CashMovementRequestV1 {
                amount_minor: 10_000,
                reason_code: "PETTY_CASH".to_owned(),
            },
            None,
        )
        .unwrap();
    assert_eq!(after_out.expected_cash.formatted, "1400.00");

    assert_eq!(
        client
            .cash_in(
                CashMovementRequestV1 {
                    amount_minor: 1_000,
                    reason_code: "  ".to_owned(),
                },
                None,
            )
            .expect_err("a blank reason is not an audit trail")
            .code(),
        "CASH_REASON_REQUIRED"
    );
}

#[test]
fn a_safe_drop_needs_a_second_identity_and_then_moves_the_money_once() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    open_till(&client);

    let drop = client
        .request_safe_drop(
            CashMovementRequestV1 {
                amount_minor: 80_000,
                reason_code: "DRAWER_LIMIT".to_owned(),
            },
            None,
        )
        .unwrap();
    assert_eq!(drop.status, "PENDING_APPROVAL");
    // Pending moves nothing: the money is still in the drawer.
    assert_eq!(
        client.cash_position().unwrap().expected_cash.formatted,
        "1000.00"
    );

    // The requester cannot approve their own request.
    assert_eq!(
        client
            .approve_safe_drop(
                ApproveSafeDropRequestV1 {
                    safe_drop_id: drop.safe_drop_id.clone(),
                    approver: "CASHIER-001".to_owned(),
                    approve: true,
                },
                None,
            )
            .expect_err("self-approval is refused")
            .code(),
        "SAFE_DROP_SELF_APPROVAL"
    );

    let approved = client
        .approve_safe_drop(
            ApproveSafeDropRequestV1 {
                safe_drop_id: drop.safe_drop_id.clone(),
                approver: "SUPERVISOR-01".to_owned(),
                approve: true,
            },
            None,
        )
        .unwrap();
    assert_eq!(approved.status, "APPROVED");
    assert_eq!(approved.approved_by.as_deref(), Some("SUPERVISOR-01"));
    let position = client.cash_position().unwrap();
    assert_eq!(position.expected_cash.formatted, "200.00");
    assert_eq!(position.safe_drops.formatted, "800.00");
    assert!(position.pending_safe_drops.is_empty());

    // Approving again is idempotent.
    client
        .approve_safe_drop(
            ApproveSafeDropRequestV1 {
                safe_drop_id: drop.safe_drop_id,
                approver: "SUPERVISOR-01".to_owned(),
                approve: true,
            },
            None,
        )
        .unwrap();
    assert_eq!(
        client.cash_position().unwrap().expected_cash.formatted,
        "200.00"
    );
}

/// The contract half of the blind close: the payload a cashier's close path
/// receives cannot carry the expected cash, by construction (ADR 022).
#[test]
fn the_pre_close_payload_cannot_carry_the_expected_cash() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    open_till(&client);
    cash_sale(&client, SALE);

    let requirements = client.request_session_close(None).unwrap();
    assert!(requirements.requires_declaration);
    assert!(requirements.blockers.is_empty());

    // No amount-bearing key exists on the type at all.
    let rendered = serde_json::to_value(&requirements).unwrap();
    let object = rendered.as_object().unwrap();
    for key in object.keys() {
        assert!(
            !["expected_cash", "expected", "amount", "total", "movements"].contains(&key.as_str()),
            "the pre-close payload must not carry `{key}`"
        );
    }
    // And no value in it equals the figure, ids excluded — a UUID can contain
    // any digits by chance.
    let scanned: String = object
        .iter()
        .filter(|(key, _)| key.as_str() != "session_id")
        .map(|(_, value)| value.to_string())
        .collect();
    for forbidden in ["111600", "1116.00", "100000", "1000.00", "11600"] {
        assert!(
            !scanned.contains(forbidden),
            "the pre-close payload leaked {forbidden}: {scanned}"
        );
    }

    // Only the declaration reveals it.
    let result = client
        .declare_closing_cash(
            DeclareClosingCashRequestV1 {
                counted_minor: FLOAT + SALE - 1_500,
            },
            None,
        )
        .unwrap();
    assert_eq!(result.closure_kind, "DECLARED");
    assert_eq!(result.expected_at_close.formatted, "1116.00");
    assert_eq!(result.declared_cash.as_ref().unwrap().formatted, "1101.00");
    assert_eq!(result.variance.as_ref().unwrap().minor, -1_500);
    assert_eq!(result.variance_label, "SHORTAGE");
}

#[test]
fn a_declaration_cannot_be_sent_before_a_close_is_requested() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    open_till(&client);

    // Declaring in one step would side-step the ordering that makes the close
    // blind, so the contract refuses it.
    assert_eq!(
        client
            .declare_closing_cash(
                DeclareClosingCashRequestV1 {
                    counted_minor: FLOAT,
                },
                None,
            )
            .expect_err("a declaration answers a request")
            .code(),
        "SESSION_DECLARATION_REQUIRED"
    );
}

#[test]
fn reopening_a_session_creates_a_new_one_linked_to_the_closed_one() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    open_till(&client);
    let original = client.get_edge_state().unwrap().session.unwrap().session_id;
    client.request_session_close(None).unwrap();
    client
        .declare_closing_cash(
            DeclareClosingCashRequestV1 {
                counted_minor: FLOAT - 2_000,
            },
            None,
        )
        .unwrap();

    let reopened = client
        .reopen_session(
            ReopenSessionRequestV1 {
                session_id: original.clone(),
                reason: "recount".to_owned(),
                operator_id: "CASHIER-002".to_owned(),
            },
            None,
        )
        .unwrap();
    assert_ne!(reopened.session_id, original);
    assert_eq!(reopened.status, "OPEN");
    assert_eq!(reopened.operator_id, "CASHIER-002");
    // The float is what was counted, not what was expected.
    assert_eq!(reopened.opening_cash.formatted, "980.00");
    assert_eq!(
        client.cash_position().unwrap().expected_cash.formatted,
        "980.00"
    );
}

#[test]
fn a_forced_end_of_day_closes_the_till_without_inventing_a_count() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    open_till(&client);
    cash_sale(&client, SALE);

    // A normal close is refused while the till is open, and says why.
    let blocked = client
        .close_business_day(None)
        .expect_err("a drawer nobody counted");
    assert_eq!(blocked.code(), "BUSINESS_DAY_CLOSE_BLOCKED");

    let forced = client
        .force_close_business_day(
            ForceCloseBusinessDayRequestV1 {
                reason: "central EOD".to_owned(),
                actor: "OPERATIONS".to_owned(),
            },
            None,
        )
        .unwrap();
    assert_eq!(forced.day.status, "CLOSED");

    let state = client.get_edge_state().unwrap();
    assert!(state.business_day.is_none());
    assert!(state.session.is_none());

    // The journal keeps the actor, the reason and the fact that it was forced.
    let events = client.get_journal(100).unwrap();
    let forced = events
        .iter()
        .find(|event| event.event_type == "BUSINESS_DAY_FORCE_CLOSED")
        .expect("a forced close is journalled as such");
    assert_eq!(forced.payload["reason"], "central EOD");
    assert_eq!(forced.payload["actor"], "OPERATIONS");
    let session_closed = events
        .iter()
        .find(|event| event.event_type == "SESSION_CLOSED")
        .unwrap();
    assert_eq!(session_closed.payload["forced"], true);
    // No counted cash was fabricated, and the variance is unknown, not zero.
    assert_eq!(
        session_closed.payload["declared_minor"],
        serde_json::Value::Null
    );
    assert_eq!(
        session_closed.payload["variance_minor"],
        serde_json::Value::Null
    );
    assert_eq!(session_closed.payload["expected_minor"], 111_600);
}

#[test]
fn cash_movements_are_journalled_with_their_reason() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    open_till(&client);
    client
        .cash_in(
            CashMovementRequestV1 {
                amount_minor: 25_000,
                reason_code: "CHANGE_FUND".to_owned(),
            },
            None,
        )
        .unwrap();

    let event = client
        .get_journal(50)
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == "CASH_MOVEMENT_RECORDED")
        .expect("a drawer movement is journalled");
    assert_eq!(event.payload["movement_type"], "CASH_IN");
    assert_eq!(event.payload["direction"], "IN");
    assert_eq!(event.payload["amount_minor"], 25_000);
    assert_eq!(event.payload["reason_code"], "CHANGE_FUND");
    assert_eq!(event.payload["operator_id"], "CASHIER-001");
}
