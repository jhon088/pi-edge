//! Deterministic protocol output for cash management.
//!
//! Frozen clock and sequential ids, so the drawer a client sees — including a
//! pending drop, a variance and a forced close — is byte-stable.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::ports::EdgeStore;
use edge_golden_tests::*;
use edge_protocol::{CashPositionV1, SafeDropV1, SessionCloseRequirementsV1, SessionCloseResultV1};

const FLOAT: i64 = 100_000;
const SALE: i64 = 11_600;

/// The drawer after a float, an overpaid cash sale, a top-up and a payout.
#[test]
fn golden_cash_position() {
    let core = golden_core();
    open_register_with_float(&core, FLOAT);
    // 200.00 tendered for a 116.00 sale: 84.00 goes back.
    golden_cash_sale(&core, 20_000);
    core.cash_in(CashInCommand {
        request_id: None,
        amount_minor: 50_000,
        reason_code: "CHANGE_FUND".to_owned(),
    })
    .unwrap();
    let view = core
        .cash_out(CashOutCommand {
            request_id: None,
            amount_minor: 10_000,
            reason_code: "PETTY_CASH".to_owned(),
        })
        .unwrap();

    // 1000 + 116 + 500 − 100 = 1516.00
    assert_eq!(view.position.expected_cash.minor(), 151_600);
    assert_eq!(view.position.tendered_total.minor(), 20_000);
    assert_eq!(view.position.change_total.minor(), 8_400);
    assert_golden(
        "cash_position",
        &serde_json::to_value(CashPositionV1::from(&view)).unwrap(),
    );
}

/// A requested drop, before anyone has decided it: expected cash unchanged.
#[test]
fn golden_safe_drop_pending() {
    let core = golden_core();
    open_register_with_float(&core, FLOAT);
    let drop = core
        .request_safe_drop(RequestSafeDropCommand {
            request_id: None,
            amount_minor: 80_000,
            reason_code: "DRAWER_LIMIT".to_owned(),
        })
        .unwrap();

    assert_eq!(
        core.get_cash_position()
            .unwrap()
            .position
            .expected_cash
            .minor(),
        FLOAT,
        "a pending drop moves nothing"
    );
    assert_golden(
        "safe_drop_pending",
        &serde_json::to_value(SafeDropV1::from(&drop)).unwrap(),
    );
}

/// The same drop once a supervisor has approved it.
#[test]
fn golden_safe_drop_approved() {
    let core = golden_core();
    open_register_with_float(&core, FLOAT);
    let drop = core
        .request_safe_drop(RequestSafeDropCommand {
            request_id: None,
            amount_minor: 80_000,
            reason_code: "DRAWER_LIMIT".to_owned(),
        })
        .unwrap();
    let approved = core
        .approve_safe_drop(ApproveSafeDropCommand {
            request_id: None,
            safe_drop_id: drop.safe_drop_id,
            approver: edge_domain::prelude::OperatorId::new("SUPERVISOR-01").unwrap(),
            approve: true,
        })
        .unwrap();

    assert_eq!(
        core.get_cash_position()
            .unwrap()
            .position
            .expected_cash
            .minor(),
        FLOAT - 80_000
    );
    assert_golden(
        "safe_drop_approved",
        &serde_json::to_value(SafeDropV1::from(&approved)).unwrap(),
    );
}

/// The pre-close payload, pinned so a field that leaks the expected cash
/// cannot be added without this file changing (ADR 022).
#[test]
fn golden_pre_close_requirements() {
    let core = golden_core();
    open_register_with_float(&core, FLOAT);
    golden_cash_sale(&core, SALE);
    let requirements = core
        .request_session_close(RequestSessionCloseCommand::default())
        .unwrap();

    let dto = SessionCloseRequirementsV1::from(&requirements);
    let rendered = serde_json::to_value(&dto).unwrap();
    // The expected figure is 1116.00 and it appears nowhere.
    let scanned: String = rendered
        .as_object()
        .unwrap()
        .iter()
        .filter(|(key, _)| key.as_str() != "session_id")
        .map(|(_, value)| value.to_string())
        .collect();
    for forbidden in ["111600", "1116.00"] {
        assert!(!scanned.contains(forbidden));
    }
    assert_golden("pre_close_requirements", &rendered);
}

/// An exact count: no variance.
#[test]
fn golden_blind_close_exact() {
    let core = golden_core();
    open_register_with_float(&core, FLOAT);
    golden_cash_sale(&core, SALE);
    core.request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    let result = core
        .declare_closing_cash(DeclareClosingCashCommand {
            request_id: None,
            counted_minor: FLOAT + SALE,
        })
        .unwrap();

    assert_eq!(result.variance.map(|v| v.minor()), Some(0));
    assert_golden(
        "blind_close_exact",
        &serde_json::to_value(SessionCloseResultV1::from(&result)).unwrap(),
    );
}

/// A shortage: the variance is recorded, not corrected.
#[test]
fn golden_blind_close_variance() {
    let core = golden_core();
    open_register_with_float(&core, FLOAT);
    golden_cash_sale(&core, SALE);
    core.request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    let result = core
        .declare_closing_cash(DeclareClosingCashCommand {
            request_id: None,
            counted_minor: FLOAT + SALE - 2_500,
        })
        .unwrap();

    assert_eq!(result.variance.map(|v| v.minor()), Some(-2_500));
    assert_eq!(result.variance_label, "SHORTAGE");
    assert_golden(
        "blind_close_variance",
        &serde_json::to_value(SessionCloseResultV1::from(&result)).unwrap(),
    );
}

/// A forced close: an expected figure, no count, and an unknown variance that
/// must never render as zero (ADR 024).
#[test]
fn golden_forced_close() {
    let core = golden_core();
    open_register_with_float(&core, FLOAT);
    golden_cash_sale(&core, SALE);
    let session_id = core.get_edge_state().unwrap().session.unwrap().id();
    core.force_close_business_day(ForceCloseBusinessDayCommand {
        request_id: None,
        reason: "central EOD".to_owned(),
        actor: "OPERATIONS".to_owned(),
    })
    .unwrap();

    let declaration = core
        .store()
        .transact(|uow| uow.find_close_declaration(session_id))
        .unwrap()
        .unwrap();
    assert!(declaration.declared_cash.is_none());
    assert!(declaration.variance.is_none());
    assert_golden(
        "forced_close_declaration",
        &serde_json::to_value(&declaration).unwrap(),
    );
}
