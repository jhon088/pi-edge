//! Cross-aggregate invariants enforced by the use cases.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::AppError;
use edge_domain::prelude::*;
use edge_integration_tests::*;
use uuid::Uuid;

#[test]
fn refuses_to_work_before_the_device_is_provisioned() {
    let core = in_memory_core();
    assert!(matches!(
        core.open_business_day(OpenBusinessDayCommand::default()),
        Err(AppError::DeviceNotInitialized)
    ));
}

#[test]
fn refuses_two_business_days_for_the_same_store() {
    let core = in_memory_core();
    init_device(&core);
    core.open_business_day(OpenBusinessDayCommand::default())
        .unwrap();
    assert!(matches!(
        core.open_business_day(OpenBusinessDayCommand::default()),
        Err(AppError::BusinessDayAlreadyOpen { .. })
    ));
}

#[test]
fn refuses_two_open_sessions_on_the_same_register() {
    let core = in_memory_core();
    open_register(&core);
    let second = core.open_register_session(OpenRegisterSessionCommand {
        request_id: None,
        register_id: RegisterId::new("REG-001").unwrap(),
        operator_id: OperatorId::new("CASHIER-002").unwrap(),
        opening_cash_minor: 0,
    });
    assert!(matches!(
        second,
        Err(AppError::RegisterSessionAlreadyOpen { .. })
    ));
}

#[test]
fn refuses_a_session_without_an_open_business_day() {
    let core = in_memory_core();
    init_device(&core);
    assert!(matches!(
        core.open_register_session(OpenRegisterSessionCommand {
            request_id: None,
            register_id: RegisterId::new("REG-001").unwrap(),
            operator_id: OperatorId::new("CASHIER-001").unwrap(),
            opening_cash_minor: 0,
        }),
        Err(AppError::NoOpenBusinessDay)
    ));
}

#[test]
fn refuses_a_sale_without_an_open_session() {
    let core = in_memory_core();
    init_device(&core);
    core.open_business_day(OpenBusinessDayCommand::default())
        .unwrap();
    assert!(matches!(
        core.start_transaction(StartTransactionCommand::default()),
        Err(AppError::NoOpenRegisterSession)
    ));
}

#[test]
fn refuses_two_active_transactions_on_one_session() {
    let core = in_memory_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    assert!(matches!(
        core.start_transaction(StartTransactionCommand::default()),
        Err(AppError::TransactionAlreadyActive { .. })
    ));
}

#[test]
fn refuses_payment_on_a_transaction_that_is_not_totalized() {
    let core = in_memory_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    let result = core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: Tender::Cash,
        amount_minor: 11_600,
    });
    assert!(matches!(
        result,
        Err(AppError::Domain(DomainError::InvalidTransition { .. }))
    ));
}

#[test]
fn refuses_to_finalize_an_underpaid_sale() {
    let core = in_memory_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(&core, 1_000);
    let result = core.finalize_transaction(FinalizeTransactionCommand::default());
    assert!(matches!(
        result,
        Err(AppError::Domain(DomainError::InvalidTransition { .. }))
    ));
    // Nothing leaked into the outbox.
    assert_eq!(core.get_edge_state().unwrap().pending_outbox, 0);
}

#[test]
fn refuses_to_totalize_an_empty_sale() {
    let core = in_memory_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    assert!(matches!(
        core.totalize_transaction(TotalizeTransactionCommand::default()),
        Err(AppError::Domain(DomainError::EmptyTransaction))
    ));
}

#[test]
fn refuses_to_close_a_session_with_an_active_transaction() {
    let core = in_memory_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    assert!(matches!(
        core.close_register_session(CloseRegisterSessionCommand::default()),
        Err(AppError::SessionHasActiveTransaction { .. })
    ));
}

#[test]
fn refuses_to_close_a_business_day_with_open_sessions() {
    let core = in_memory_core();
    open_register(&core);
    assert!(matches!(
        core.close_business_day(CloseBusinessDayCommand::default()),
        Err(AppError::BusinessDayHasOpenSessions { count: 1 })
    ));
}

#[test]
fn replaying_a_request_id_does_not_apply_the_command_twice() {
    let core = in_memory_core();
    init_device(&core);
    let request_id = Some(Uuid::from_u128(0xA11CE));

    let first = core
        .open_business_day(OpenBusinessDayCommand { request_id })
        .unwrap();
    let replay = core
        .open_business_day(OpenBusinessDayCommand { request_id })
        .unwrap();

    assert_eq!(first.id(), replay.id());
    assert_eq!(first.state(), replay.state());
    // One business day, one journal event: the replay was a no-op.
    assert_eq!(journal_types(&core), vec!["BUSINESS_DAY_OPENED"]);
}

#[test]
fn item_added_after_totalize_invalidates_the_official_totals() {
    let core = in_memory_core();
    init_device(&core);
    pricing_catalog(1).activate(&core).unwrap();
    core.open_business_day(OpenBusinessDayCommand::default())
        .unwrap();
    core.open_register_session(OpenRegisterSessionCommand {
        request_id: None,
        register_id: RegisterId::new("REG-001").unwrap(),
        operator_id: OperatorId::new("CASHIER-001").unwrap(),
        opening_cash_minor: 100_000,
    })
    .unwrap();
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let reopened = core.add_item(item("SKU-002", 1)).unwrap();
    assert_eq!(reopened.status(), TransactionStatus::Open);
    assert_eq!(reopened.state().totals.grand_total.minor(), 0);

    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(totalized.state().totals.grand_total.minor(), 17_400);
}
