//! Cash state survives a process death.
//!
//! Each case does something to a drawer, throws the application object away,
//! builds a brand new one against the same database, and checks that what
//! comes back is exactly what was written — and that nothing was applied
//! twice or quietly completed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::ports::EdgeStore;
use edge_cash::SafeDropStatus;
use edge_crash_recovery_tests::*;
use edge_domain::prelude::*;

const FLOAT: i64 = 100_000;
const SALE: i64 = 11_600;

#[test]
fn a_cash_payment_leaves_an_exact_position_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    {
        let core = core_at(&db, 1);
        open_register_with_float(&core, FLOAT);
        // 200.00 tendered for a 116.00 sale: 84.00 back out of the drawer.
        cash_sale(&core, 20_000);
        core.cash_out(CashOutCommand {
            request_id: None,
            amount_minor: 5_000,
            reason_code: "PETTY_CASH".to_owned(),
        })
        .unwrap();
    }

    let core = core_at(&db, 1_000);
    let view = core.get_cash_position().unwrap();
    assert_eq!(view.position.opening_float.minor(), FLOAT);
    assert_eq!(view.position.cash_sales.minor(), SALE);
    assert_eq!(view.position.cash_out.minor(), 5_000);
    assert_eq!(view.position.expected_cash.minor(), FLOAT + SALE - 5_000);
    // The tender audit survives too, without distorting the expectation.
    assert_eq!(view.position.tendered_total.minor(), 20_000);
    assert_eq!(view.position.change_total.minor(), 8_400);
    assert_eq!(view.movements.len(), 3);
}

#[test]
fn a_pending_safe_drop_is_still_pending_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let safe_drop_id;
    {
        let core = core_at(&db, 1);
        open_register_with_float(&core, FLOAT);
        safe_drop_id = core
            .request_safe_drop(RequestSafeDropCommand {
                request_id: None,
                amount_minor: 80_000,
                reason_code: "DRAWER_LIMIT".to_owned(),
            })
            .unwrap()
            .safe_drop_id;
    }

    let core = core_at(&db, 1_000);
    let view = core.get_cash_position().unwrap();
    // A restart does not decide an approval, and the money has not moved.
    assert_eq!(view.pending_safe_drops.len(), 1);
    assert_eq!(
        view.pending_safe_drops[0].status,
        SafeDropStatus::PendingApproval
    );
    assert_eq!(view.position.expected_cash.minor(), FLOAT);
    assert_eq!(view.position.safe_drops.minor(), 0);

    // And it still blocks a close until someone decides it.
    let requirements = core
        .request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    assert!(requirements
        .blockers
        .iter()
        .any(|blocker| blocker == &format!("PENDING_SAFE_DROP:{safe_drop_id}")));
}

#[test]
fn an_approved_safe_drop_is_not_applied_twice_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let safe_drop_id;
    {
        let core = core_at(&db, 1);
        open_register_with_float(&core, FLOAT);
        safe_drop_id = core
            .request_safe_drop(RequestSafeDropCommand {
                request_id: None,
                amount_minor: 80_000,
                reason_code: "DRAWER_LIMIT".to_owned(),
            })
            .unwrap()
            .safe_drop_id;
        core.approve_safe_drop(ApproveSafeDropCommand {
            request_id: None,
            safe_drop_id,
            approver: OperatorId::new("SUPERVISOR-01").unwrap(),
            approve: true,
        })
        .unwrap();
    }

    let core = core_at(&db, 1_000);
    assert_eq!(
        core.get_cash_position()
            .unwrap()
            .position
            .expected_cash
            .minor(),
        FLOAT - 80_000
    );
    // Re-approving across the restart changes nothing: the movement is keyed
    // by the drop, so the drawer cannot move twice (ADR 021).
    core.approve_safe_drop(ApproveSafeDropCommand {
        request_id: None,
        safe_drop_id,
        approver: OperatorId::new("SUPERVISOR-01").unwrap(),
        approve: true,
    })
    .unwrap();
    let view = core.get_cash_position().unwrap();
    assert_eq!(view.position.expected_cash.minor(), FLOAT - 80_000);
    assert_eq!(view.position.safe_drops.minor(), 80_000);
    assert_eq!(view.movements.len(), 2, "float plus exactly one drop");
}

#[test]
fn a_session_awaiting_its_declaration_is_still_awaiting_it_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    {
        let core = core_at(&db, 1);
        open_register_with_float(&core, FLOAT);
        cash_sale(&core, SALE);
        core.request_session_close(RequestSessionCloseCommand::default())
            .unwrap();
    }

    let core = core_at(&db, 1_000);
    let session = core.get_edge_state().unwrap().session.unwrap();
    assert_eq!(session.status(), RegisterSessionStatus::CloseRequested);
    // No declaration was invented by the restart.
    let declaration = core
        .store()
        .transact(|uow| uow.find_close_declaration(session.id()))
        .unwrap();
    assert!(declaration.is_none());
    // The day cannot close over it.
    assert_eq!(
        core.close_business_day(CloseBusinessDayCommand::default())
            .unwrap_err()
            .kind(),
        "BUSINESS_DAY_CLOSE_BLOCKED"
    );

    // And the declaration still works, against the recovered expectation.
    let result = core
        .declare_closing_cash(DeclareClosingCashCommand {
            request_id: None,
            counted_minor: FLOAT + SALE,
        })
        .unwrap();
    assert_eq!(result.expected_at_close.minor(), FLOAT + SALE);
    assert_eq!(result.variance.map(|v| v.minor()), Some(0));
}

#[test]
fn a_declaration_survives_a_restart_without_being_duplicated() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let session_id;
    {
        let core = core_at(&db, 1);
        open_register_with_float(&core, FLOAT);
        cash_sale(&core, SALE);
        core.request_session_close(RequestSessionCloseCommand::default())
            .unwrap();
        let result = core
            .declare_closing_cash(DeclareClosingCashCommand {
                request_id: None,
                counted_minor: FLOAT + SALE - 2_500,
            })
            .unwrap();
        session_id = result.session_id;
        assert_eq!(result.variance.map(|v| v.minor()), Some(-2_500));
    }

    let core = core_at(&db, 1_000);
    let declaration = core
        .store()
        .transact(|uow| uow.find_close_declaration(session_id))
        .unwrap()
        .unwrap();
    assert_eq!(
        declaration.declared_cash.map(|m| m.minor()),
        Some(FLOAT + SALE - 2_500)
    );
    assert_eq!(declaration.expected_at_close.minor(), FLOAT + SALE);
    assert_eq!(declaration.variance.map(|m| m.minor()), Some(-2_500));
    assert_eq!(declaration.variance_label(), "SHORTAGE");

    // The session is closed and stays closed; the day can now end normally.
    let session = core
        .store()
        .transact(|uow| uow.find_register_session(session_id))
        .unwrap()
        .unwrap();
    assert_eq!(session.status(), RegisterSessionStatus::Closed);
    assert_eq!(session.state().closure_kind, Some(ClosureKind::Declared));
    let day = core
        .close_business_day(CloseBusinessDayCommand::default())
        .unwrap();
    assert!(!day.was_forced());
}

#[test]
fn a_forced_close_survives_a_restart_with_its_unknown_variance() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let session_id;
    {
        let core = core_at(&db, 1);
        open_register_with_float(&core, FLOAT);
        cash_sale(&core, SALE);
        session_id = core.get_edge_state().unwrap().session.unwrap().id();
        core.force_close_business_day(ForceCloseBusinessDayCommand {
            request_id: None,
            reason: "central EOD".to_owned(),
            actor: "OPERATIONS".to_owned(),
        })
        .unwrap();
    }

    let core = core_at(&db, 1_000);
    // The day is over and says so, with its reason and its actor.
    assert!(core.get_edge_state().unwrap().business_day.is_none());
    let declaration = core
        .store()
        .transact(|uow| uow.find_close_declaration(session_id))
        .unwrap()
        .unwrap();
    assert!(declaration.declared_cash.is_none(), "no count was invented");
    assert!(declaration.variance.is_none(), "unknown, not zero");
    assert_eq!(declaration.expected_at_close.minor(), FLOAT + SALE);
    assert_eq!(declaration.variance_label(), "UNDECLARED");
    // And the ledger that produced that expectation is intact.
    let movements = core
        .store()
        .transact(|uow| uow.list_cash_movements(session_id))
        .unwrap();
    assert_eq!(movements.len(), 2);
}
