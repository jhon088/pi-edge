//! Cash management against real SQLite.
//!
//! The arithmetic and the state machines are pinned by unit tests in
//! `edge-cash`; these check what only the whole runtime shows: that a cash
//! payment moves the drawer exactly once, that a pending safe drop moves
//! nothing, that the blind close cannot be short-circuited, and that a forced
//! end of day invents no count.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::ports::EdgeStore;
use edge_application::AppError;
use edge_cash::{Direction, MovementType, SafeDropStatus};
use edge_domain::prelude::*;
use edge_integration_tests::*;
use edge_payments::TenderType;

/// The demo sale: 100.00 at 16% = 116.00.
const SALE: i64 = 11_600;
const FLOAT: i64 = 100_000;

fn drop_request(core: &TestCore, amount_minor: i64) -> uuid::Uuid {
    core.request_safe_drop(RequestSafeDropCommand {
        request_id: None,
        amount_minor,
        reason_code: "DRAWER_LIMIT".to_owned(),
    })
    .unwrap()
    .safe_drop_id
}

// --------------------------------------------------------------- the ledger

#[test]
fn opening_a_session_writes_its_float_as_the_first_movement() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    let view = core.get_cash_position().unwrap();

    // The ledger alone explains the drawer: the float is a movement, not just
    // a column on the session.
    assert_eq!(view.movements.len(), 1);
    assert_eq!(view.movements[0].movement_type, MovementType::OpeningFloat);
    assert_eq!(view.movements[0].direction, Direction::In);
    assert_eq!(view.position.opening_float.minor(), FLOAT);
    assert_eq!(view.position.expected_cash.minor(), FLOAT);
}

#[test]
fn a_cash_sale_increases_expected_cash_by_what_stayed_in_the_drawer() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    cash_sale(&core, SALE);
    let view = core.get_cash_position().unwrap();

    assert_eq!(view.position.cash_sales.minor(), SALE);
    assert_eq!(view.position.expected_cash.minor(), FLOAT + SALE);
    assert_eq!(view.movements.len(), 2);
    let sale = &view.movements[1];
    assert_eq!(sale.movement_type, MovementType::CashSale);
    assert_eq!(sale.tendered.map(|m| m.minor()), Some(SALE));
    assert_eq!(sale.change.map(|m| m.minor()), Some(0));
}

#[test]
fn a_tendered_overpayment_leaves_only_the_net_in_the_drawer() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    // 200.00 handed over for a 116.00 sale: 84.00 goes back out of the drawer.
    cash_sale(&core, 20_000);
    let view = core.get_cash_position().unwrap();

    // The expectation is the net, so the change does not inflate the drawer.
    assert_eq!(view.position.expected_cash.minor(), FLOAT + SALE);
    assert_eq!(view.position.cash_sales.minor(), SALE);
    // And the audit of what actually crossed the counter is kept.
    assert_eq!(view.position.tendered_total.minor(), 20_000);
    assert_eq!(view.position.change_total.minor(), 8_400);
    let sale = &view.movements[1];
    assert_eq!(sale.amount.minor(), SALE);
    assert_eq!(sale.tendered.map(|m| m.minor()), Some(20_000));
    assert_eq!(sale.change.map(|m| m.minor()), Some(8_400));
}

#[test]
fn cash_in_and_cash_out_move_the_expectation_both_ways() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);

    let after_in = core
        .cash_in(CashInCommand {
            request_id: None,
            amount_minor: 50_000,
            reason_code: "CHANGE_FUND".to_owned(),
        })
        .unwrap();
    assert_eq!(after_in.position.expected_cash.minor(), FLOAT + 50_000);

    let after_out = core
        .cash_out(CashOutCommand {
            request_id: None,
            amount_minor: 10_000,
            reason_code: "PETTY_CASH".to_owned(),
        })
        .unwrap();
    assert_eq!(
        after_out.position.expected_cash.minor(),
        FLOAT + 50_000 - 10_000
    );
    assert_eq!(after_out.position.cash_in.minor(), 50_000);
    assert_eq!(after_out.position.cash_out.minor(), 10_000);
    assert_eq!(after_out.movements.len(), 3);
}

#[test]
fn a_manual_movement_must_state_a_reason_and_a_positive_amount() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);

    assert_eq!(
        core.cash_in(CashInCommand {
            request_id: None,
            amount_minor: 1_000,
            reason_code: "   ".to_owned(),
        })
        .unwrap_err()
        .kind(),
        "CASH_REASON_REQUIRED"
    );
    assert_eq!(
        core.cash_out(CashOutCommand {
            request_id: None,
            amount_minor: 0,
            reason_code: "PETTY_CASH".to_owned(),
        })
        .unwrap_err()
        .kind(),
        "CASH_INVALID_AMOUNT"
    );
    // Neither attempt left a movement behind.
    assert_eq!(core.get_cash_position().unwrap().movements.len(), 1);
}

#[test]
fn many_movements_accumulate_in_order() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    for index in 1..=5 {
        core.cash_in(CashInCommand {
            request_id: None,
            amount_minor: index * 1_000,
            reason_code: format!("TOP_UP_{index}"),
        })
        .unwrap();
    }
    let view = core.get_cash_position().unwrap();
    assert_eq!(view.movements.len(), 6);
    assert_eq!(
        view.position.cash_in.minor(),
        1_000 + 2_000 + 3_000 + 4_000 + 5_000
    );
    assert_eq!(view.position.expected_cash.minor(), FLOAT + 15_000);
    assert_eq!(view.position.movement_count, 6);
}

#[test]
fn a_replayed_movement_command_moves_the_drawer_once() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    let request_id = uuid::Uuid::from_u128(0xCA5);
    let first = core
        .cash_in(CashInCommand {
            request_id: Some(request_id),
            amount_minor: 25_000,
            reason_code: "CHANGE_FUND".to_owned(),
        })
        .unwrap();
    let replay = core
        .cash_in(CashInCommand {
            request_id: Some(request_id),
            amount_minor: 25_000,
            reason_code: "CHANGE_FUND".to_owned(),
        })
        .unwrap();
    assert_eq!(first.position, replay.position);
    assert_eq!(replay.movements.len(), 2, "one float, one top-up");
    assert_eq!(replay.position.expected_cash.minor(), FLOAT + 25_000);
}

#[test]
fn the_cash_position_survives_a_fresh_application_object() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi-edge.db");
    {
        let core = core_with_store(edge_storage::SqliteEdgeStore::open(&path).unwrap());
        open_register_with_float(&core, FLOAT);
        cash_sale(&core, 20_000);
        core.cash_out(CashOutCommand {
            request_id: None,
            amount_minor: 5_000,
            reason_code: "PETTY_CASH".to_owned(),
        })
        .unwrap();
    }
    let core = core_with_store(edge_storage::SqliteEdgeStore::open(&path).unwrap());
    let view = core.get_cash_position().unwrap();
    assert_eq!(view.position.expected_cash.minor(), FLOAT + SALE - 5_000);
    assert_eq!(view.movements.len(), 3);
    assert_eq!(view.position.tendered_total.minor(), 20_000);
}

// ---------------------------------------------- payment / ledger integration

#[test]
fn a_card_payment_moves_no_cash() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Card,
        amount_minor: SALE,
    })
    .unwrap();

    let view = core.get_cash_position().unwrap();
    assert_eq!(view.movements.len(), 1, "only the opening float");
    assert_eq!(view.position.expected_cash.minor(), FLOAT);
}

#[test]
fn a_mixed_sale_moves_only_its_cash_half() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Cash,
        amount_minor: 3_000,
    })
    .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Card,
        amount_minor: 8_600,
    })
    .unwrap();

    let view = core.get_cash_position().unwrap();
    assert_eq!(view.position.cash_sales.minor(), 3_000);
    assert_eq!(view.position.expected_cash.minor(), FLOAT + 3_000);
    assert_eq!(view.movements.len(), 2);
}

#[test]
fn a_replayed_cash_payment_does_not_move_the_drawer_twice() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let request_id = uuid::Uuid::from_u128(0xBEEF);
    for _ in 0..2 {
        core.start_payment(StartPaymentCommand {
            request_id: Some(request_id),
            tender_type: TenderType::Cash,
            amount_minor: SALE,
        })
        .unwrap();
    }
    let view = core.get_cash_position().unwrap();
    assert_eq!(view.movements.len(), 2, "float plus exactly one sale");
    assert_eq!(view.position.expected_cash.minor(), FLOAT + SALE);
}

#[test]
fn reversing_a_card_payment_moves_no_cash() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    let paid = core
        .start_payment(StartPaymentCommand {
            request_id: None,
            tender_type: TenderType::Card,
            amount_minor: SALE,
        })
        .unwrap();
    core.reverse_payment(ReversePaymentCommand {
        request_id: None,
        payment_id: paid.payment.payment.payment_id,
    })
    .unwrap();

    // Nothing left the drawer, so nothing returns to it.
    let view = core.get_cash_position().unwrap();
    assert_eq!(view.movements.len(), 1);
    assert_eq!(view.position.expected_cash.minor(), FLOAT);
}

// ------------------------------------------------------------- safe drops

#[test]
fn a_pending_safe_drop_does_not_reduce_expected_cash() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    let drop = core
        .request_safe_drop(RequestSafeDropCommand {
            request_id: None,
            amount_minor: 80_000,
            reason_code: "DRAWER_LIMIT".to_owned(),
        })
        .unwrap();

    assert_eq!(drop.status, SafeDropStatus::PendingApproval);
    let view = core.get_cash_position().unwrap();
    // The money is still in the drawer until someone with authority says it
    // left (ADR 023).
    assert_eq!(view.position.expected_cash.minor(), FLOAT);
    assert_eq!(view.position.safe_drops.minor(), 0);
    assert_eq!(view.pending_safe_drops.len(), 1);
    assert_eq!(view.movements.len(), 1);
}

#[test]
fn the_requester_cannot_approve_their_own_drop() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    let safe_drop_id = drop_request(&core, 80_000);

    let error = core
        .approve_safe_drop(ApproveSafeDropCommand {
            request_id: None,
            safe_drop_id,
            // The session's own operator requested it.
            approver: OperatorId::new("CASHIER-001").unwrap(),
            approve: true,
        })
        .unwrap_err();
    assert_eq!(error.kind(), "SAFE_DROP_SELF_APPROVAL");
    // And nothing moved.
    assert_eq!(
        core.get_cash_position()
            .unwrap()
            .position
            .expected_cash
            .minor(),
        FLOAT
    );
}

#[test]
fn an_approved_drop_reduces_expected_cash_exactly_once() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    let safe_drop_id = drop_request(&core, 80_000);

    let approved = core
        .approve_safe_drop(ApproveSafeDropCommand {
            request_id: None,
            safe_drop_id,
            approver: supervisor(),
            approve: true,
        })
        .unwrap();
    assert_eq!(approved.status, SafeDropStatus::Approved);
    assert_eq!(
        approved.approved_by.as_ref().map(ToString::to_string),
        Some("SUPERVISOR-01".to_owned())
    );

    let view = core.get_cash_position().unwrap();
    assert_eq!(view.position.safe_drops.minor(), 80_000);
    assert_eq!(view.position.expected_cash.minor(), FLOAT - 80_000);
    assert!(view.pending_safe_drops.is_empty());
    let drop_movement = view
        .movements
        .iter()
        .find(|m| m.movement_type == MovementType::SafeDrop)
        .unwrap();
    assert_eq!(drop_movement.direction, Direction::Out);
    // The movement names the approval that authorised it.
    assert_eq!(drop_movement.approval_id, Some(approved.approval_id));

    // Approving again changes nothing.
    core.approve_safe_drop(ApproveSafeDropCommand {
        request_id: None,
        safe_drop_id,
        approver: supervisor(),
        approve: true,
    })
    .unwrap();
    let again = core.get_cash_position().unwrap();
    assert_eq!(again.position.expected_cash.minor(), FLOAT - 80_000);
    assert_eq!(again.movements.len(), view.movements.len());
}

#[test]
fn a_rejected_drop_leaves_the_money_where_it_was() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    let safe_drop_id = drop_request(&core, 80_000);
    let rejected = core
        .approve_safe_drop(ApproveSafeDropCommand {
            request_id: None,
            safe_drop_id,
            approver: supervisor(),
            approve: false,
        })
        .unwrap();
    assert_eq!(rejected.status, SafeDropStatus::Rejected);
    let view = core.get_cash_position().unwrap();
    assert_eq!(view.position.expected_cash.minor(), FLOAT);
    assert!(view.pending_safe_drops.is_empty());
    assert_eq!(view.movements.len(), 1);
}

// ------------------------------------------------------------- blind close

#[test]
fn the_pre_close_answer_cannot_reveal_the_expected_cash() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    cash_sale(&core, SALE);

    let requirements = core
        .request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    // Structural, not visual: there is no field that could hold the figure.
    let rendered = serde_json::to_value(&requirements).unwrap();
    let object = rendered.as_object().unwrap();
    for key in object.keys() {
        assert!(
            !["expected_cash", "expected", "amount", "total", "movements"].contains(&key.as_str()),
            "`{key}` must not exist on the pre-close payload"
        );
    }
    let scanned = object
        .iter()
        .filter(|(key, _)| key.as_str() != "session_id")
        .map(|(_, value)| value.to_string())
        .collect::<String>();
    for forbidden in ["111600", "1116.00", "100000"] {
        assert!(
            !scanned.contains(forbidden),
            "leaked {forbidden}: {scanned}"
        );
    }
    assert!(requirements.requires_declaration);
    assert!(requirements.can_declare());
    assert_eq!(requirements.movement_count, 2);
}

#[test]
fn an_exact_declaration_closes_with_no_variance() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    cash_sale(&core, SALE);
    core.request_session_close(RequestSessionCloseCommand::default())
        .unwrap();

    let result = core
        .declare_closing_cash(DeclareClosingCashCommand {
            request_id: None,
            counted_minor: FLOAT + SALE,
        })
        .unwrap();
    assert_eq!(result.closure_kind, ClosureKind::Declared);
    assert_eq!(result.declared_cash.map(|m| m.minor()), Some(FLOAT + SALE));
    assert_eq!(result.expected_at_close.minor(), FLOAT + SALE);
    assert_eq!(result.variance.map(|m| m.minor()), Some(0));
    assert_eq!(result.variance_label, "BALANCED");
    // And the session is closed, with the declaration on it.
    let state = core.get_edge_state().unwrap();
    assert!(state.session.is_none());
}

#[test]
fn a_shortage_is_negative_and_an_overage_positive() {
    for (counted, expected_variance, label) in [
        (FLOAT + SALE - 1_000, -1_000, "SHORTAGE"),
        (FLOAT + SALE + 500, 500, "OVERAGE"),
    ] {
        let core = in_memory_core();
        open_register_with_float(&core, FLOAT);
        cash_sale(&core, SALE);
        core.request_session_close(RequestSessionCloseCommand::default())
            .unwrap();
        let result = core
            .declare_closing_cash(DeclareClosingCashCommand {
                request_id: None,
                counted_minor: counted,
            })
            .unwrap();
        assert_eq!(result.variance.map(|m| m.minor()), Some(expected_variance));
        assert_eq!(result.variance_label, label);
        // Neither is corrected automatically: the evidence stands.
        assert_eq!(result.expected_at_close.minor(), FLOAT + SALE);
    }
}

#[test]
fn a_declaration_cannot_be_made_without_requesting_a_close_first() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    // Declaring straight away would let a client skip the ordering that makes
    // the close blind (ADR 022).
    let error = core
        .declare_closing_cash(DeclareClosingCashCommand {
            request_id: None,
            counted_minor: FLOAT,
        })
        .unwrap_err();
    assert_eq!(error.kind(), "SESSION_DECLARATION_REQUIRED");
}

#[test]
fn a_declaration_cannot_be_changed_silently() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    core.request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    core.declare_closing_cash(DeclareClosingCashCommand {
        request_id: None,
        counted_minor: FLOAT - 500,
    })
    .unwrap();

    // The session is closed, so there is nothing left to declare against.
    let error = core
        .declare_closing_cash(DeclareClosingCashCommand {
            request_id: None,
            counted_minor: FLOAT,
        })
        .unwrap_err();
    assert!(
        matches!(error, AppError::NoOpenRegisterSession),
        "unexpected error: {error:?}"
    );
}

#[test]
fn the_expected_figure_is_materialised_so_later_movements_cannot_move_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi-edge.db");
    let declaration;
    {
        let core = core_with_store(edge_storage::SqliteEdgeStore::open(&path).unwrap());
        open_register_with_float(&core, FLOAT);
        cash_sale(&core, SALE);
        core.request_session_close(RequestSessionCloseCommand::default())
            .unwrap();
        declaration = core
            .declare_closing_cash(DeclareClosingCashCommand {
                request_id: None,
                counted_minor: FLOAT,
            })
            .unwrap();
        assert_eq!(declaration.expected_at_close.minor(), FLOAT + SALE);
        assert_eq!(declaration.variance.map(|m| m.minor()), Some(-SALE));
    }
    // A new session on the same register, with its own movements, must not
    // change yesterday's variance. The id offset keeps this lifetime's ids
    // clear of the first one's, as a restarted daemon's would be.
    let core = core_two_days_later(&path);
    let reopened = core
        .reopen_session(ReopenSessionCommand {
            request_id: None,
            session_id: declaration.session_id,
            reason: "recount".to_owned(),
            operator_id: OperatorId::new("CASHIER-002").unwrap(),
        })
        .unwrap();
    core.cash_in(CashInCommand {
        request_id: None,
        amount_minor: 90_000,
        reason_code: "CHANGE_FUND".to_owned(),
    })
    .unwrap();
    let stored = core
        .store()
        .transact(|uow| uow.find_close_declaration(declaration.session_id))
        .unwrap()
        .unwrap();
    assert_eq!(stored.expected_at_close.minor(), FLOAT + SALE);
    assert_eq!(stored.variance.map(|m| m.minor()), Some(-SALE));
    assert_ne!(reopened.id(), declaration.session_id);
}

// ----------------------------------------------------------- session close

#[test]
fn a_close_is_blocked_by_an_active_sale_and_by_a_pending_drop() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    drop_request(&core, 10_000);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();

    let requirements = core
        .request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    assert!(!requirements.can_declare());
    assert!(requirements
        .blockers
        .iter()
        .any(|b| b.starts_with("ACTIVE_TRANSACTION:")));
    assert!(requirements
        .blockers
        .iter()
        .any(|b| b.starts_with("PENDING_SAFE_DROP:")));
    // And the session stays OPEN rather than half-closing.
    assert_eq!(
        core.get_edge_state().unwrap().session.unwrap().status(),
        RegisterSessionStatus::Open
    );
}

#[test]
fn a_close_is_blocked_by_an_unresolved_payment() {
    let core = scripted_core(
        edge_payments::SimulatorScript::approving()
            .defaulting_to(edge_payments::SimulatedOutcome::Unknown),
    );
    init_device(&core);
    edge_application::testing::CatalogFixture::demo(1)
        .activate(&core)
        .unwrap();
    core.open_business_day(OpenBusinessDayCommand::default())
        .unwrap();
    core.open_register_session(OpenRegisterSessionCommand {
        request_id: None,
        register_id: RegisterId::new("REG-001").unwrap(),
        operator_id: OperatorId::new("CASHIER-001").unwrap(),
        opening_cash_minor: FLOAT,
    })
    .unwrap();
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(AddItemCommand {
        request_id: None,
        selector: edge_catalog::ProductSelector::Barcode(
            edge_catalog::Barcode::new(edge_application::testing::DEMO_BARCODE).unwrap(),
        ),
        quantity: Quantity::from_units(1).unwrap(),
    })
    .unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Card,
        amount_minor: SALE,
    })
    .unwrap();

    let requirements = core
        .request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    assert!(requirements
        .blockers
        .iter()
        .any(|b| b == "UNRESOLVED_PAYMENT"));
}

#[test]
fn a_register_can_run_several_sequential_sessions_and_a_cashier_several_registers() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    core.request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    core.declare_closing_cash(DeclareClosingCashCommand {
        request_id: None,
        counted_minor: FLOAT,
    })
    .unwrap();

    // Same register, a second session, a different cashier.
    core.open_register_session(OpenRegisterSessionCommand {
        request_id: None,
        register_id: RegisterId::new("REG-001").unwrap(),
        operator_id: OperatorId::new("CASHIER-002").unwrap(),
        opening_cash_minor: 50_000,
    })
    .unwrap();
    // And the same cashier on another register at the same time: identity
    // belongs to the session, not to the register.
    core.open_register_session(OpenRegisterSessionCommand {
        request_id: None,
        register_id: RegisterId::new("REG-002").unwrap(),
        operator_id: OperatorId::new("CASHIER-002").unwrap(),
        opening_cash_minor: 20_000,
    })
    .unwrap();

    let day = core.get_edge_state().unwrap().business_day.unwrap();
    let sessions = core
        .store()
        .transact(|uow| uow.list_sessions_for_day(day.id()))
        .unwrap();
    assert_eq!(sessions.len(), 3);
    assert_eq!(
        sessions
            .iter()
            .filter(|s| s.status() == RegisterSessionStatus::Closed)
            .count(),
        1
    );
}

// --------------------------------------------------------------- reopening

#[test]
fn reopening_a_session_keeps_the_previous_close_intact() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    cash_sale(&core, SALE);
    core.request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    let closed = core
        .declare_closing_cash(DeclareClosingCashCommand {
            request_id: None,
            counted_minor: FLOAT + SALE - 1_000,
        })
        .unwrap();

    let reopened = core
        .reopen_session(ReopenSessionCommand {
            request_id: None,
            session_id: closed.session_id,
            reason: "miscount at close".to_owned(),
            operator_id: OperatorId::new("CASHIER-002").unwrap(),
        })
        .unwrap();

    // A new session, linked to the old one.
    assert_ne!(reopened.id(), closed.session_id);
    assert_eq!(
        reopened.state().reopened_from_session_id,
        Some(closed.session_id)
    );
    // Its float is what was *counted*, not what was expected.
    assert_eq!(reopened.state().opening_cash.minor(), FLOAT + SALE - 1_000);
    assert_eq!(reopened.state().operator_id.to_string(), "CASHIER-002");

    // The previous close still says exactly what it said.
    let previous = core
        .store()
        .transact(|uow| uow.find_register_session(closed.session_id))
        .unwrap()
        .unwrap();
    assert_eq!(previous.status(), RegisterSessionStatus::Closed);
    assert_eq!(previous.state().closure_kind, Some(ClosureKind::Declared));
    let declaration = core
        .store()
        .transact(|uow| uow.find_close_declaration(closed.session_id))
        .unwrap()
        .unwrap();
    assert_eq!(declaration.variance.map(|m| m.minor()), Some(-1_000));

    // And the new session's ledger starts from its own float.
    let view = core.get_cash_position().unwrap();
    assert_eq!(view.position.expected_cash.minor(), FLOAT + SALE - 1_000);
    assert_eq!(view.movements.len(), 1);
}

#[test]
fn reopening_is_journalled_with_its_reason_and_refuses_an_open_session() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    core.request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    let closed = core
        .declare_closing_cash(DeclareClosingCashCommand {
            request_id: None,
            counted_minor: FLOAT,
        })
        .unwrap();
    core.reopen_session(ReopenSessionCommand {
        request_id: None,
        session_id: closed.session_id,
        reason: "recount".to_owned(),
        operator_id: OperatorId::new("CASHIER-002").unwrap(),
    })
    .unwrap();

    let event = core
        .get_journal(50)
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == JournalEventType::SessionReopened)
        .expect("the reopen is journalled");
    assert_eq!(event.payload_json["reason"], "recount");
    assert_eq!(
        event.payload_json["reopened_from_session_id"],
        closed.session_id.to_string()
    );
    assert_eq!(event.payload_json["operator_id"], "CASHIER-002");

    // A reason is mandatory, and an already-open session cannot be reopened.
    assert!(core
        .reopen_session(ReopenSessionCommand {
            request_id: None,
            session_id: closed.session_id,
            reason: "  ".to_owned(),
            operator_id: OperatorId::new("CASHIER-002").unwrap(),
        })
        .is_err());
}

// ------------------------------------------------------------ business day

#[test]
fn a_business_day_keeps_its_identity_across_midnight() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi-edge.db");
    let day_id;
    {
        let core = core_with_store(edge_storage::SqliteEdgeStore::open(&path).unwrap());
        open_register_with_float(&core, FLOAT);
        day_id = core.get_edge_state().unwrap().business_day.unwrap().id();
    }
    // A lifetime that believes it is two days later.
    let core = core_two_days_later(&path);
    let state = core.get_edge_state().unwrap();
    assert_eq!(
        state.business_day.unwrap().id(),
        day_id,
        "a business day is an operational period, never a calendar date"
    );
}

#[test]
fn a_normal_close_needs_every_session_closed() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);

    // Open session.
    let error = core
        .close_business_day(CloseBusinessDayCommand::default())
        .unwrap_err();
    assert_eq!(error.kind(), "BUSINESS_DAY_CLOSE_BLOCKED");

    // Awaiting declaration is still a blocker: a count that was started and
    // never declared is unfinished business.
    core.request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    match core
        .close_business_day(CloseBusinessDayCommand::default())
        .unwrap_err()
    {
        AppError::BusinessDayCloseBlocked { blockers } => assert!(blockers
            .iter()
            .any(|b| b.starts_with("SESSION_AWAITING_DECLARATION:"))),
        other => panic!("unexpected error: {other:?}"),
    }

    core.declare_closing_cash(DeclareClosingCashCommand {
        request_id: None,
        counted_minor: FLOAT,
    })
    .unwrap();
    let day = core
        .close_business_day(CloseBusinessDayCommand::default())
        .unwrap();
    assert_eq!(day.status(), BusinessDayStatus::Closed);
    assert!(!day.was_forced());
}

#[test]
fn a_forced_close_ends_the_day_without_inventing_a_count() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    cash_sale(&core, SALE);
    let session_id = core.get_edge_state().unwrap().session.unwrap().id();

    let day = core
        .force_close_business_day(ForceCloseBusinessDayCommand {
            request_id: None,
            reason: "central EOD".to_owned(),
            actor: "OPERATIONS".to_owned(),
        })
        .unwrap();

    assert_eq!(day.status(), BusinessDayStatus::Closed);
    assert!(day.was_forced());
    assert_eq!(day.state().close_reason.as_deref(), Some("central EOD"));
    assert_eq!(day.state().closed_by.as_deref(), Some("OPERATIONS"));

    // The session was closed as forced, with no fabricated count.
    let session = core
        .store()
        .transact(|uow| uow.find_register_session(session_id))
        .unwrap()
        .unwrap();
    assert_eq!(session.status(), RegisterSessionStatus::Closed);
    assert_eq!(session.state().closure_kind, Some(ClosureKind::Forced));
    let declaration = core
        .store()
        .transact(|uow| uow.find_close_declaration(session_id))
        .unwrap()
        .unwrap();
    assert!(
        declaration.declared_cash.is_none(),
        "a forced close must not fabricate a count"
    );
    assert!(
        declaration.variance.is_none(),
        "the variance is unknown, not zero"
    );
    assert_eq!(declaration.variance_label(), "UNDECLARED");
    // The expected figure is still recorded: it is a fact about the ledger.
    assert_eq!(declaration.expected_at_close.minor(), FLOAT + SALE);
    assert_eq!(declaration.reason.as_deref(), Some("central EOD"));
}

/// Forcing a day closed must not have a built-in ceiling. How many registers
/// a store runs is the store's business, not a constant of the core.
#[test]
fn a_forced_close_handles_far_more_sessions_than_any_hardcoded_batch() {
    let core = in_memory_core();
    init_device(&core);
    activate_demo_catalog(&core);
    core.open_business_day(OpenBusinessDayCommand::default())
        .unwrap();
    // Well past the 32 the first implementation pre-minted ids for. The number
    // is not a new limit: it is evidence that there is no limit.
    const REGISTERS: usize = 50;
    for index in 0..REGISTERS {
        core.open_register_session(OpenRegisterSessionCommand {
            request_id: None,
            register_id: RegisterId::new(format!("REG-{index:03}")).unwrap(),
            operator_id: OperatorId::new(format!("CASHIER-{index:03}")).unwrap(),
            opening_cash_minor: 1_000 + index as i64,
        })
        .unwrap();
    }
    let day_id = core.get_edge_state().unwrap().business_day.unwrap().id();

    let day = core
        .force_close_business_day(ForceCloseBusinessDayCommand {
            request_id: None,
            reason: "central EOD".to_owned(),
            actor: "OPERATIONS".to_owned(),
        })
        .unwrap();
    assert!(day.was_forced());

    // Every till was closed as forced, each with its own declaration and its
    // own expected figure — and none with a fabricated count.
    let sessions = core
        .store()
        .transact(|uow| uow.list_sessions_for_day(day_id))
        .unwrap();
    assert_eq!(sessions.len(), REGISTERS);
    for (index, session) in sessions.iter().enumerate() {
        assert_eq!(session.status(), RegisterSessionStatus::Closed);
        assert_eq!(session.state().closure_kind, Some(ClosureKind::Forced));
        let declaration = core
            .store()
            .transact(|uow| uow.find_close_declaration(session.id()))
            .unwrap()
            .expect("every forced session has a declaration");
        assert!(declaration.declared_cash.is_none());
        assert!(declaration.variance.is_none());
        assert_eq!(
            declaration.expected_at_close.minor(),
            1_000 + index as i64,
            "each session keeps its own expected figure"
        );
    }
    // And the journal names all of them.
    let event = core
        .get_journal(500)
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == JournalEventType::BusinessDayForceClosed)
        .unwrap();
    assert_eq!(
        event.payload_json["forced_sessions"]
            .as_array()
            .unwrap()
            .len(),
        REGISTERS
    );
}

#[test]
fn a_forced_close_is_distinguishable_and_keeps_its_actor_and_reason() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    core.force_close_business_day(ForceCloseBusinessDayCommand {
        request_id: None,
        reason: "store evacuated".to_owned(),
        actor: "SUPERVISOR-01".to_owned(),
    })
    .unwrap();

    let events = core.get_journal(50).unwrap();
    let day_event = events
        .iter()
        .find(|event| event.event_type == JournalEventType::BusinessDayForceClosed)
        .expect("a forced day close is journalled as such");
    assert_eq!(day_event.payload_json["reason"], "store evacuated");
    assert_eq!(day_event.payload_json["actor"], "SUPERVISOR-01");
    assert_eq!(
        day_event.payload_json["forced_sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let session_event = events
        .iter()
        .find(|event| event.event_type == JournalEventType::SessionClosed)
        .unwrap();
    assert_eq!(session_event.payload_json["forced"], true);
    assert_eq!(
        session_event.payload_json["declared_minor"],
        serde_json::Value::Null
    );
    assert_eq!(
        session_event.payload_json["variance_minor"],
        serde_json::Value::Null
    );
}

#[test]
fn a_forced_close_must_state_a_reason_and_an_actor() {
    for (reason, actor) in [("  ", "OPERATIONS"), ("central EOD", "  ")] {
        let core = in_memory_core();
        open_register_with_float(&core, FLOAT);
        assert!(core
            .force_close_business_day(ForceCloseBusinessDayCommand {
                request_id: None,
                reason: reason.to_owned(),
                actor: actor.to_owned(),
            })
            .is_err());
        // And the day is still open: a refusal changes nothing.
        assert!(core.get_edge_state().unwrap().business_day.is_some());
    }
}

#[test]
fn a_forced_close_destroys_no_evidence() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    cash_sale(&core, 20_000);
    let safe_drop_id = drop_request(&core, 5_000);
    let session_id = core.get_edge_state().unwrap().session.unwrap().id();

    core.force_close_business_day(ForceCloseBusinessDayCommand {
        request_id: None,
        reason: "central EOD".to_owned(),
        actor: "OPERATIONS".to_owned(),
    })
    .unwrap();

    // The ledger, the sale, the payment and the pending approval all survive.
    let (movements, pending) = core
        .store()
        .transact(|uow| {
            Ok((
                uow.list_cash_movements(session_id)?,
                uow.list_pending_safe_drops(session_id)?,
            ))
        })
        .unwrap();
    assert_eq!(movements.len(), 2);
    assert_eq!(pending.len(), 1, "the exception is kept, not resolved");
    assert_eq!(pending[0].safe_drop_id, safe_drop_id);
}

#[test]
fn only_one_business_day_is_open_per_store() {
    let core = in_memory_core();
    open_register_with_float(&core, FLOAT);
    assert!(core
        .open_business_day(OpenBusinessDayCommand::default())
        .is_err());
}
