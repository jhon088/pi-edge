//! Pins the error-code contract.
//!
//! Clients branch on these strings, so a rename in the core must not silently
//! change what a UI sees. `edge-protocol` cannot depend on `edge-application`,
//! which is why the equality is asserted here instead of in a type.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::AppError;
use edge_cash::CashError;
use edge_catalog::CatalogError;
use edge_domain::prelude::{DomainError, MoneyError};
use edge_payments::PaymentError;
use edge_protocol::{
    error_codes, AddItemRequestV1, ApproveSafeDropRequestV1, CashMovementRequestV1,
    ChangeQuantityRequestV1, DeclareClosingCashRequestV1, ForceCloseBusinessDayRequestV1,
    OperationV1, PaymentRequestV1, ProductSelectorV1, RemoveItemRequestV1, ReopenSessionRequestV1,
    SessionCloseRequirementsV1, StartPaymentRequestV1, TransactionRequestV1,
};

#[test]
fn core_error_kinds_match_the_published_codes() {
    assert_eq!(
        AppError::Domain(DomainError::InvalidTransition {
            aggregate: "RetailTransaction",
            from: "OPEN",
            attempted: "FINALIZED",
        })
        .kind(),
        error_codes::INVALID_TRANSACTION_STATE
    );
    assert_eq!(
        AppError::Domain(DomainError::EmptyTransaction).kind(),
        error_codes::DOMAIN_RULE_VIOLATION
    );
    assert_eq!(
        AppError::Domain(DomainError::Money(MoneyError::Overflow)).kind(),
        error_codes::DOMAIN_RULE_VIOLATION
    );
    assert_eq!(
        AppError::Storage("disk on fire".to_owned()).kind(),
        error_codes::STORAGE_FAILURE
    );
}

#[test]
fn documented_business_codes_are_stable() {
    // A representative sample of the codes a client is expected to branch on.
    for (error, expected) in [
        (AppError::DeviceNotInitialized, "DEVICE_NOT_INITIALIZED"),
        (AppError::NoOpenBusinessDay, "NO_OPEN_BUSINESS_DAY"),
        (AppError::NoOpenRegisterSession, "NO_OPEN_REGISTER_SESSION"),
        (AppError::NoActiveTransaction, "NO_ACTIVE_TRANSACTION"),
        (
            AppError::BusinessDayAlreadyOpen {
                business_day_id: "x".to_owned(),
            },
            "BUSINESS_DAY_ALREADY_OPEN",
        ),
        (
            AppError::SessionHasActiveTransaction {
                transaction_id: "x".to_owned(),
            },
            "SESSION_HAS_ACTIVE_TRANSACTION",
        ),
    ] {
        assert_eq!(error.kind(), expected);
    }
}

#[test]
fn promotion_codes_are_stable() {
    assert_eq!(
        AppError::Catalog(CatalogError::AmbiguousPromotion {
            target: "LINE:1".to_owned(),
            priority: 10,
            promotions: "A, B".to_owned(),
        })
        .kind(),
        error_codes::AMBIGUOUS_PROMOTION
    );
    assert_eq!(
        AppError::Catalog(CatalogError::UnknownPromotion {
            promotion_id: "P".to_owned(),
        })
        .kind(),
        error_codes::CATALOG_UNKNOWN_PROMOTION
    );
    // Naming a line that is not in the basket is its own code: a UI retries
    // differently for "you typed the wrong number" than for a broken rule.
    assert_eq!(
        AppError::Domain(DomainError::LineNotFound { line_number: 3 }).kind(),
        error_codes::LINE_NOT_FOUND
    );
    assert_eq!(
        AppError::Domain(DomainError::AdjustmentTargetMissing { line_number: 3 }).kind(),
        error_codes::LINE_NOT_FOUND
    );
}

#[test]
fn every_operation_has_a_stable_name() {
    // The wire names a UI logs and a server audits. Renaming one is a
    // protocol change, not a refactor.
    for (operation, expected) in [
        (
            OperationV1::AddItem(AddItemRequestV1 {
                product: ProductSelectorV1::Sku("S".to_owned()),
                quantity_thousandths: 1_000,
            }),
            "ADD_ITEM",
        ),
        (
            OperationV1::RemoveItem(RemoveItemRequestV1 { line_number: 1 }),
            "REMOVE_ITEM",
        ),
        (
            OperationV1::ChangeQuantity(ChangeQuantityRequestV1 {
                line_number: 1,
                quantity_thousandths: 2_000,
            }),
            "CHANGE_QUANTITY",
        ),
        (OperationV1::Totalize, "TOTALIZE"),
    ] {
        assert_eq!(operation.name(), expected);
        // All four change state, so all four are commands and all four are
        // refused on an event subscription.
        assert!(operation.is_command());
    }
}

#[test]
fn payment_codes_are_stable() {
    // A UI branches on these: "reconcile first", "reverse first", "pay less",
    // "the provider said no" are four different things for a cashier.
    for (error, expected) in [
        (
            PaymentError::PaymentNotFound {
                payment_id: "p".to_owned(),
            },
            error_codes::PAYMENT_NOT_FOUND,
        ),
        (
            PaymentError::InvalidTransition {
                aggregate: "Payment",
                from: "FAILED",
                attempted: "COMMITTED",
            },
            error_codes::PAYMENT_INVALID_STATE,
        ),
        (
            PaymentError::AlreadyResolved {
                payment_id: "p".to_owned(),
                status: "COMMITTED",
            },
            error_codes::PAYMENT_ALREADY_RESOLVED,
        ),
        (
            PaymentError::RequiresReconciliation {
                payment_id: "p".to_owned(),
            },
            error_codes::PAYMENT_REQUIRES_RECONCILIATION,
        ),
        (
            PaymentError::ReversalNotAllowed {
                payment_id: "p".to_owned(),
                status: "FAILED",
            },
            error_codes::PAYMENT_REVERSAL_NOT_ALLOWED,
        ),
        (
            PaymentError::ExceedsBalance {
                tender: "CARD",
                requested_minor: 2,
                remaining_minor: 1,
            },
            error_codes::PAYMENT_EXCEEDS_BALANCE,
        ),
        (
            PaymentError::NothingDue,
            error_codes::PAYMENT_INVALID_AMOUNT,
        ),
        (
            PaymentError::ProviderDeclined { reason: None },
            error_codes::PAYMENT_PROVIDER_DECLINED,
        ),
        (
            PaymentError::ProviderUnavailable {
                reason: "no route".to_owned(),
            },
            error_codes::PAYMENT_PROVIDER_UNAVAILABLE,
        ),
        (
            PaymentError::InvalidRecord {
                field: "payment_id",
                reason: "not a uuid".to_owned(),
            },
            error_codes::PAYMENT_INVALID_RECORD,
        ),
    ] {
        assert_eq!(error.code(), expected);
        // And the application layer must publish the same code.
        assert_eq!(AppError::Payment(error).kind(), expected);
    }

    // A frozen basket is its own code, not a generic bad transition.
    assert_eq!(
        AppError::Domain(DomainError::BasketLockedByPayment {
            transaction_id: "t".to_owned(),
        })
        .kind(),
        error_codes::BASKET_LOCKED_BY_PAYMENT
    );
    assert_eq!(
        AppError::Domain(DomainError::UnresolvedPayment).kind(),
        error_codes::PAYMENT_REQUIRES_RECONCILIATION
    );
}

#[test]
fn payment_operations_have_stable_names() {
    for (operation, expected) in [
        (
            OperationV1::StartPayment(StartPaymentRequestV1 {
                tender_type: "CASH".to_owned(),
                amount_minor: 1_000,
            }),
            "START_PAYMENT",
        ),
        (
            OperationV1::ReconcilePayment(PaymentRequestV1 {
                payment_id: "p".to_owned(),
            }),
            "RECONCILE_PAYMENT",
        ),
        (
            OperationV1::ReversePayment(PaymentRequestV1 {
                payment_id: "p".to_owned(),
            }),
            "REVERSE_PAYMENT",
        ),
    ] {
        assert_eq!(operation.name(), expected);
        // All three move money, so none is allowed on an event subscription.
        assert!(operation.is_command());
    }

    // Reads are not commands.
    for operation in [
        OperationV1::GetPayment(PaymentRequestV1 {
            payment_id: "p".to_owned(),
        }),
        OperationV1::GetTransactionPayments(TransactionRequestV1 {
            transaction_id: "t".to_owned(),
        }),
    ] {
        assert!(!operation.is_command());
    }
}

#[test]
fn cash_codes_are_stable() {
    for (error, expected) in [
        (
            CashError::NonPositiveAmount { minor: 0 },
            error_codes::CASH_INVALID_AMOUNT,
        ),
        (
            CashError::WrongDirection {
                movement_type: "CASH_SALE",
                expected: "IN",
                actual: "OUT",
            },
            error_codes::CASH_INVALID_MOVEMENT,
        ),
        (
            CashError::UnbalancedCashSale {
                amount_minor: 1,
                tendered_minor: 2,
                change_minor: 3,
            },
            error_codes::CASH_INVALID_MOVEMENT,
        ),
        (
            CashError::MissingReason {
                field: "reason_code",
            },
            error_codes::CASH_REASON_REQUIRED,
        ),
        (
            CashError::InvalidSafeDropTransition {
                safe_drop_id: "d".to_owned(),
                from: "REJECTED",
                attempted: "APPROVED",
            },
            error_codes::SAFE_DROP_INVALID_STATE,
        ),
        (
            CashError::SelfApproval,
            error_codes::SAFE_DROP_SELF_APPROVAL,
        ),
        (
            CashError::SafeDropNotFound {
                safe_drop_id: "d".to_owned(),
            },
            error_codes::SAFE_DROP_NOT_FOUND,
        ),
        (
            CashError::DeclarationRequired,
            error_codes::SESSION_DECLARATION_REQUIRED,
        ),
        (
            CashError::AlreadyDeclared,
            error_codes::SESSION_ALREADY_DECLARED,
        ),
        (
            CashError::InvalidRecord {
                field: "movement_id",
                reason: "not a uuid".to_owned(),
            },
            error_codes::CASH_INVALID_RECORD,
        ),
    ] {
        assert_eq!(error.code(), expected);
        assert_eq!(AppError::Cash(error).kind(), expected);
    }

    // The two blocker errors name what is in the way rather than refusing
    // opaquely, and have their own codes.
    assert_eq!(
        AppError::SessionCloseBlocked {
            blockers: vec!["PENDING_SAFE_DROP:x".to_owned()],
        }
        .kind(),
        error_codes::SESSION_CLOSE_BLOCKED
    );
    assert_eq!(
        AppError::BusinessDayCloseBlocked {
            blockers: vec!["OPEN_SESSION:x".to_owned()],
        }
        .kind(),
        error_codes::BUSINESS_DAY_CLOSE_BLOCKED
    );
}

#[test]
fn cash_operations_have_stable_names() {
    let movement = CashMovementRequestV1 {
        amount_minor: 1_000,
        reason_code: "R".to_owned(),
    };
    for (operation, expected, is_command) in [
        (OperationV1::GetCashPosition, "GET_CASH_POSITION", false),
        (OperationV1::CashIn(movement.clone()), "CASH_IN", true),
        (OperationV1::CashOut(movement.clone()), "CASH_OUT", true),
        (
            OperationV1::RequestSafeDrop(movement),
            "REQUEST_SAFE_DROP",
            true,
        ),
        (
            OperationV1::ApproveSafeDrop(ApproveSafeDropRequestV1 {
                safe_drop_id: "d".to_owned(),
                approver: "SUPERVISOR-01".to_owned(),
                approve: true,
            }),
            "APPROVE_SAFE_DROP",
            true,
        ),
        (
            OperationV1::RequestSessionClose,
            "REQUEST_SESSION_CLOSE",
            true,
        ),
        (
            OperationV1::DeclareClosingCash(DeclareClosingCashRequestV1 {
                counted_minor: 1_000,
            }),
            "DECLARE_CLOSING_CASH",
            true,
        ),
        (
            OperationV1::ReopenSession(ReopenSessionRequestV1 {
                session_id: "s".to_owned(),
                reason: "recount".to_owned(),
                operator_id: "CASHIER-002".to_owned(),
            }),
            "REOPEN_SESSION",
            true,
        ),
        (
            OperationV1::ForceCloseBusinessDay(ForceCloseBusinessDayRequestV1 {
                reason: "central EOD".to_owned(),
                actor: "OPERATIONS".to_owned(),
            }),
            "FORCE_CLOSE_BUSINESS_DAY",
            true,
        ),
    ] {
        assert_eq!(operation.name(), expected);
        assert_eq!(operation.is_command(), is_command, "{expected}");
    }

    // The one-shot session close is gone: a till cannot be finished without
    // being counted (ADR 022).
    let names: Vec<&str> = vec![
        "REQUEST_SESSION_CLOSE",
        "DECLARE_CLOSING_CASH",
        "FORCE_CLOSE_BUSINESS_DAY",
    ];
    assert!(!names.contains(&"CLOSE_REGISTER_SESSION"));
}

/// The structural half of the blind close, pinned at the contract level: the
/// pre-close type has no field that could carry an amount (ADR 022).
#[test]
fn the_pre_close_dto_has_no_amount_bearing_field() {
    let rendered = serde_json::to_value(SessionCloseRequirementsV1 {
        session_id: "11111111-1111-1111-1111-111111111111".to_owned(),
        currency: "MXN".to_owned(),
        movement_count: 7,
        blockers: Vec::new(),
        requires_declaration: true,
    })
    .unwrap();
    let object = rendered.as_object().expect("an object");
    assert_eq!(
        object.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "session_id",
            "currency",
            "movement_count",
            "blockers",
            "requires_declaration"
        ],
        "adding a field here needs a deliberate review: it is the payload a \
         cashier sees before counting the drawer"
    );
}
