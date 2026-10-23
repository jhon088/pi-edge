//! Pins the error-code contract.
//!
//! Clients branch on these strings, so a rename in the core must not silently
//! change what a UI sees. `edge-protocol` cannot depend on `edge-application`,
//! which is why the equality is asserted here instead of in a type.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::AppError;
use edge_cash::CashError;
use edge_catalog::CatalogError;
use edge_domain::prelude::{DomainError, MoneyError, TransactionStatus};
use edge_hardware::HardwareError;
use edge_inventory::InventoryError;
use edge_payments::PaymentError;
use edge_protocol::{
    error_codes, AddItemRequestV1, ApproveSafeDropRequestV1, CashMovementRequestV1,
    ChangeQuantityRequestV1, CreateRefundRequestV1, DeclareClosingCashRequestV1,
    ForceCloseBusinessDayRequestV1, ImportInventoryBootstrapRequestV1, InventoryMovementRequestV1,
    InventoryQueryV1, OperationV1, PaymentRequestV1, PrintTransactionRequestV1, ProductSelectorV1,
    RefundRequestV1, RemoveItemRequestV1, ReopenSessionRequestV1, SessionCloseRequirementsV1,
    StartPaymentRequestV1, SubmitScanRequestV1, TransactionRequestV1,
};
use edge_refunds::RefundError;
use edge_sync::SyncError;

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

#[test]
fn inventory_codes_are_stable() {
    for (error, expected) in [
        (
            InventoryError::WrongDirection {
                movement_type: "SALE",
                expected: "OUT",
                actual: "IN",
            },
            error_codes::INVENTORY_INVALID_MOVEMENT,
        ),
        (
            InventoryError::MissingReason {
                field: "reason_code",
            },
            error_codes::INVENTORY_REASON_REQUIRED,
        ),
        (
            InventoryError::UnknownProduct {
                product_ref: "SKU-GHOST".to_owned(),
            },
            error_codes::INVENTORY_UNKNOWN_PRODUCT,
        ),
        (
            InventoryError::CatalogRequired,
            error_codes::INVENTORY_CATALOG_REQUIRED,
        ),
        (
            InventoryError::AlreadyBootstrapped {
                source_snapshot_id: "WMS-1".to_owned(),
            },
            error_codes::INVENTORY_ALREADY_BOOTSTRAPPED,
        ),
        (
            InventoryError::EmptyBootstrap,
            error_codes::INVENTORY_INVALID_BOOTSTRAP,
        ),
        (
            InventoryError::DuplicateBootstrapItem {
                product_ref: "SKU-001".to_owned(),
            },
            error_codes::INVENTORY_INVALID_BOOTSTRAP,
        ),
        (
            InventoryError::InsufficientStock {
                product_ref: "SKU-001".to_owned(),
                on_hand: "1.000".to_owned(),
                requested: "2.000".to_owned(),
            },
            error_codes::INVENTORY_INSUFFICIENT_STOCK,
        ),
        (
            InventoryError::InvalidRecord {
                field: "source_ref",
                reason: "duplicate".to_owned(),
            },
            error_codes::INVENTORY_INVALID_RECORD,
        ),
    ] {
        assert_eq!(error.code(), expected);
        assert_eq!(AppError::Inventory(error).kind(), expected);
    }
}

#[test]
fn inventory_operations_have_stable_names() {
    let query = InventoryQueryV1 {
        product_ref: "SKU-001".to_owned(),
        movement_limit: 10,
        offset: 0,
    };
    let movement = InventoryMovementRequestV1 {
        product_ref: "SKU-001".to_owned(),
        quantity_thousandths: 1_000,
        reason_code: "DAMAGE".to_owned(),
    };
    for (operation, expected, is_command) in [
        (
            OperationV1::ImportInventoryBootstrap(ImportInventoryBootstrapRequestV1 {
                source_snapshot_id: "WMS-1".to_owned(),
                generated_at: "2026-10-08T14:00:00Z".to_owned(),
                items: Vec::new(),
            }),
            "IMPORT_INVENTORY_BOOTSTRAP",
            true,
        ),
        (
            OperationV1::GetInventoryPosition(query.clone()),
            "GET_INVENTORY_POSITION",
            false,
        ),
        (
            OperationV1::GetInventoryMovements(query),
            "GET_INVENTORY_MOVEMENTS",
            false,
        ),
        (
            OperationV1::InventoryIn(movement.clone()),
            "INVENTORY_IN",
            true,
        ),
        (OperationV1::InventoryOut(movement), "INVENTORY_OUT", true),
    ] {
        assert_eq!(operation.name(), expected);
        assert_eq!(operation.is_command(), is_command, "{expected}");
    }
}

/// A stock figure never crosses the wire as a float, and a direction is never
/// the sign of a quantity.
#[test]
fn an_inventory_movement_carries_a_magnitude_and_an_explicit_direction() {
    let rendered = serde_json::to_value(edge_protocol::InventoryMovementV1 {
        movement_id: "11111111-1111-1111-1111-111111111111".to_owned(),
        product_ref: "SKU-001".to_owned(),
        movement_type: "MANUAL_OUT".to_owned(),
        direction: "OUT".to_owned(),
        quantity: "0.750".to_owned(),
        quantity_thousandths: 750,
        source_ref: "req-1".to_owned(),
        transaction_id: None,
        line_number: None,
        reason_code: Some("DAMAGE".to_owned()),
        occurred_at: "2026-10-08T15:00:00Z".to_owned(),
    })
    .unwrap();

    assert_eq!(rendered["direction"], "OUT");
    // Positive even though the movement goes out: the magnitude and the
    // direction are two fields, and neither encodes the other.
    assert_eq!(rendered["quantity_thousandths"], 750);
    assert_eq!(rendered["quantity"], "0.750");
    assert!(
        rendered["quantity_thousandths"].is_i64(),
        "a quantity is an integer on the wire, in thousandths"
    );
}

#[test]
fn refund_codes_are_stable() {
    for (error, expected) in [
        (
            RefundError::OriginalNotRefundable {
                transaction_id: "t".to_owned(),
                status: "OPEN",
            },
            error_codes::REFUND_ORIGINAL_NOT_REFUNDABLE,
        ),
        // Three refusals of authority, three codes: an operator does
        // something different about each (ADR 042).
        (
            RefundError::AuthorityRequired {
                transaction_id: "t".to_owned(),
            },
            error_codes::REFUND_AUTHORITY_REQUIRED,
        ),
        (
            RefundError::AuthorityUnavailable {
                transaction_id: "t".to_owned(),
                detail: "SYNC_TIMEOUT".to_owned(),
            },
            error_codes::REFUND_AUTHORITY_UNAVAILABLE,
        ),
        (
            RefundError::NotAuthorized {
                transaction_id: "t".to_owned(),
                reason: "outside the refund window".to_owned(),
            },
            error_codes::REFUND_NOT_AUTHORIZED,
        ),
        (
            RefundError::LineNotFound { line_number: 7 },
            error_codes::REFUND_LINE_NOT_FOUND,
        ),
        (
            RefundError::DuplicateLine { line_number: 1 },
            error_codes::REFUND_INVALID_LINES,
        ),
        (RefundError::EmptyRefund, error_codes::REFUND_INVALID_LINES),
        (
            RefundError::QuantityExceeded {
                line_number: 1,
                remaining: "1.000".to_owned(),
                requested: "2.000".to_owned(),
            },
            error_codes::REFUND_QUANTITY_EXCEEDED,
        ),
        (
            RefundError::RefundInProgress {
                transaction_id: "t".to_owned(),
                refund_id: "r".to_owned(),
            },
            error_codes::REFUND_IN_PROGRESS,
        ),
        (
            RefundError::InvalidState {
                status: "CREATED",
                attempted: "COMPLETE",
            },
            error_codes::REFUND_INVALID_STATE,
        ),
        (
            RefundError::ReasonRequired,
            error_codes::REFUND_REASON_REQUIRED,
        ),
        (
            RefundError::FiscalActionInvalid {
                attempted: "NONE",
                original: "INVOICED",
            },
            error_codes::REFUND_FISCAL_ACTION_INVALID,
        ),
        (
            RefundError::TenderExceeded {
                tender: "CASH",
                remaining: "1.00".to_owned(),
                requested: "2.00".to_owned(),
            },
            error_codes::REFUND_TENDER_EXCEEDED,
        ),
        (
            RefundError::RequiresReconciliation {
                refund_id: "r".to_owned(),
            },
            error_codes::REFUND_REQUIRES_RECONCILIATION,
        ),
        (
            RefundError::InvalidRecord {
                field: "uuid",
                reason: "bad".to_owned(),
            },
            error_codes::REFUND_INVALID_RECORD,
        ),
    ] {
        assert_eq!(error.code(), expected);
        assert_eq!(AppError::Refund(error).kind(), expected);
    }
}

#[test]
fn refund_operations_have_stable_names() {
    let refund = RefundRequestV1 {
        refund_id: "11111111-1111-1111-1111-111111111111".to_owned(),
    };
    let transaction = TransactionRequestV1 {
        transaction_id: "22222222-2222-2222-2222-222222222222".to_owned(),
    };
    for (operation, expected, is_command) in [
        (
            OperationV1::GetRefundEligibility(transaction.clone()),
            "GET_REFUND_ELIGIBILITY",
            false,
        ),
        (
            OperationV1::CreateRefund(CreateRefundRequestV1 {
                transaction_id: transaction.transaction_id.clone(),
                lines: Vec::new(),
                reason_code: "CUSTOMER_RETURN".to_owned(),
                fiscal_action: "NONE".to_owned(),
            }),
            "CREATE_REFUND",
            true,
        ),
        (
            OperationV1::AcceptRefund(refund.clone()),
            "ACCEPT_REFUND",
            true,
        ),
        (
            OperationV1::StartRefundPayment(refund.clone()),
            "START_REFUND_PAYMENT",
            true,
        ),
        (
            OperationV1::ReconcileRefund(refund.clone()),
            "RECONCILE_REFUND",
            true,
        ),
        (OperationV1::GetRefund(refund), "GET_REFUND", false),
        (
            OperationV1::GetTransactionRefunds(transaction.clone()),
            "GET_TRANSACTION_REFUNDS",
            false,
        ),
    ] {
        assert_eq!(operation.name(), expected);
        assert_eq!(operation.is_command(), is_command, "{expected}");
    }

    // A refund is not a reversal: the two never share an operation (ADR 034).
    assert_ne!("RECONCILE_REFUND", "RECONCILE_PAYMENT");
    assert_ne!("START_REFUND_PAYMENT", "REVERSE_PAYMENT");

    // `transaction` is consumed by the loop above; this keeps the compiler
    // honest about it rather than leaving a dangling clone.
    drop(transaction);
}

/// The payload that opens a refund cannot assert who authorised it, and cannot
/// declare the original sale's fiscal state (ADR 030, ADR 032).
#[test]
fn the_create_refund_payload_cannot_assert_authority_or_fiscal_state() {
    let rendered = serde_json::to_value(CreateRefundRequestV1 {
        transaction_id: "22222222-2222-2222-2222-222222222222".to_owned(),
        lines: vec![edge_protocol::RefundLineRequestV1 {
            original_line_number: 1,
            quantity_thousandths: 1_000,
        }],
        reason_code: "CUSTOMER_RETURN".to_owned(),
        fiscal_action: "NONE".to_owned(),
    })
    .unwrap();
    let object = rendered.as_object().expect("an object");

    assert_eq!(
        object.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["transaction_id", "lines", "reason_code", "fiscal_action"],
        "adding a field here needs a deliberate review: it is what a till may \
         say about a refund"
    );
    // No authority, no authorization reference, no fiscal state: the runtime
    // owns the first two and the original sale owns the third.
    for forbidden in [
        "authority",
        "authorization_ref",
        "fiscal_state",
        "invoice_ref",
        "amount_minor",
        "gross_minor",
    ] {
        assert!(
            !object.contains_key(forbidden),
            "`{forbidden}` must not be something a client can state"
        );
    }
}

#[test]
fn hardware_codes_are_stable() {
    for (error, expected) in [
        (
            HardwareError::Unavailable {
                device: "printer",
                detail: "nothing is listening".to_owned(),
            },
            error_codes::HARDWARE_UNAVAILABLE,
        ),
        (
            HardwareError::Timeout {
                device: "printer",
                elapsed_ms: 2_000,
            },
            error_codes::HARDWARE_TIMEOUT,
        ),
        (
            HardwareError::Io {
                device: "printer",
                detail: "write failed".to_owned(),
            },
            error_codes::HARDWARE_IO_ERROR,
        ),
        (
            HardwareError::NotConfigured {
                device: "cash drawer",
                detail: "no bridge".to_owned(),
            },
            error_codes::HARDWARE_NOT_CONFIGURED,
        ),
        (
            HardwareError::ActionAlreadyCompleted {
                source_ref: "p1".to_owned(),
            },
            error_codes::HARDWARE_ACTION_ALREADY_COMPLETED,
        ),
    ] {
        assert_eq!(error.code(), expected);
        assert_eq!(AppError::Hardware(error).kind(), expected);
    }
    assert_eq!(
        AppError::ScanWithoutTransaction.kind(),
        error_codes::SCAN_NO_ACTIVE_TRANSACTION
    );
}

#[test]
fn hardware_operations_have_stable_names() {
    for (operation, expected, is_command) in [
        (OperationV1::GetHardwareStatus, "GET_HARDWARE_STATUS", false),
        (
            OperationV1::SubmitScan(SubmitScanRequestV1 {
                event_id: "11111111-1111-1111-1111-111111111111".to_owned(),
                barcode: "7501000000001".to_owned(),
                device_id: None,
            }),
            "SUBMIT_SCAN",
            true,
        ),
        (OperationV1::PrintTestPage, "PRINT_TEST_PAGE", true),
        (
            OperationV1::PrintTransaction(PrintTransactionRequestV1 {
                transaction_id: "22222222-2222-2222-2222-222222222222".to_owned(),
                reprint: true,
            }),
            "PRINT_TRANSACTION",
            true,
        ),
        (
            OperationV1::OpenDrawerDiagnostic,
            "OPEN_DRAWER_DIAGNOSTIC",
            true,
        ),
    ] {
        assert_eq!(operation.name(), expected);
        assert_eq!(operation.is_command(), is_command, "{expected}");
    }
}

/// The hardware surface asks for an **intention**, never for a capability.
///
/// A payload that could carry a host, a port, a driver or raw bytes would let
/// any client print anywhere on the network or send an arbitrary escape
/// sequence to a device. None of them has a field for it (ADR 039).
#[test]
fn no_hardware_payload_can_name_a_device_or_carry_bytes() {
    // The two diagnostic operations carry nothing at all.
    for operation in [
        OperationV1::PrintTestPage,
        OperationV1::OpenDrawerDiagnostic,
    ] {
        let rendered = serde_json::to_value(&operation).unwrap();
        let payload = rendered.get("payload");
        assert!(
            payload.is_none() || payload == Some(&serde_json::Value::Null),
            "{} must carry no payload: {rendered}",
            operation.name()
        );
    }

    // And the two that do carry a payload carry only what they are about.
    let print = serde_json::to_value(PrintTransactionRequestV1 {
        transaction_id: "22222222-2222-2222-2222-222222222222".to_owned(),
        reprint: false,
    })
    .unwrap();
    assert_eq!(
        print
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["transaction_id", "reprint"]
    );

    let scan = serde_json::to_value(SubmitScanRequestV1 {
        event_id: "11111111-1111-1111-1111-111111111111".to_owned(),
        barcode: "7501000000001".to_owned(),
        device_id: None,
    })
    .unwrap();
    assert_eq!(
        scan.as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["event_id", "barcode", "device_id"]
    );

    for forbidden in [
        "host", "port", "address", "driver", "bytes", "payload", "escpos", "command", "method",
        "copies", "width",
    ] {
        for rendered in [&print, &scan] {
            assert!(
                !rendered
                    .as_object()
                    .expect("an object")
                    .contains_key(forbidden),
                "`{forbidden}` must not be something a client can state"
            );
        }
    }
}

/// The sync surface: two operations, no payload, and four stable codes.
#[test]
fn sync_operations_and_codes_are_stable() {
    assert_eq!(OperationV1::GetSyncStatus.name(), "GET_SYNC_STATUS");
    assert_eq!(OperationV1::RunSyncCycle.name(), "RUN_SYNC_CYCLE");
    // Reading is safe to retry; running a cycle changes durable state.
    assert!(!OperationV1::GetSyncStatus.is_command());
    assert!(OperationV1::RunSyncCycle.is_command());

    for (error, expected) in [
        (SyncError::NotConfigured, error_codes::SYNC_NOT_CONFIGURED),
        (
            SyncError::Unavailable {
                detail: "no route".to_owned(),
            },
            error_codes::SYNC_UNAVAILABLE,
        ),
        (
            SyncError::Timeout { elapsed_ms: 30_000 },
            error_codes::SYNC_TIMEOUT,
        ),
        (
            SyncError::Malformed {
                detail: "unknown kind".to_owned(),
            },
            error_codes::SYNC_MALFORMED_RESPONSE,
        ),
    ] {
        assert_eq!(error.code(), expected);
        assert_eq!(AppError::Sync(error).kind(), expected);
    }
}

/// A sync failure is never a commercial failure.
///
/// Nothing in the sync error set can be mistaken for a reason not to sell: the
/// codes are all about reaching a server, and the register's answer to every
/// one of them is to keep the fact and carry on (ADR 040).
#[test]
fn the_sync_status_view_carries_no_address_and_no_credential() {
    let wire = serde_json::to_string(&edge_protocol::SyncStatusV1 {
        protocol_version: edge_protocol::PROTOCOL_VERSION,
        driver: "simulator".to_owned(),
        pending: 2,
        sending: 0,
        sent: 7,
        failed: 1,
        oldest_pending_at: Some("2026-10-08T15:00:00Z".to_owned()),
        next_attempt_at: None,
        inbound_cursor: Some("catalog-3-2".to_owned()),
        failed_facts: Vec::new(),
    })
    .unwrap();
    for forbidden in ["http", "token", "secret", "password", "9100"] {
        assert!(
            !wire.to_lowercase().contains(forbidden),
            "`{forbidden}` must not appear in {wire}"
        );
    }
}

/// The receipt surface: three codes, and a lifecycle a client can branch on.
#[test]
fn receipt_operations_and_codes_are_stable() {
    assert_eq!(
        OperationV1::GetReceipt(TransactionRequestV1 {
            transaction_id: "t".to_owned()
        })
        .name(),
        "GET_RECEIPT"
    );
    assert_eq!(
        OperationV1::ListRecentSales(edge_protocol::PageRequestV1 {
            limit: 10,
            offset: 0
        })
        .name(),
        "LIST_RECENT_SALES"
    );
    assert_eq!(
        OperationV1::CancelTransaction(edge_protocol::CancelTransactionRequestV1::default()).name(),
        "CANCEL_TRANSACTION"
    );
    // Reading is safe to retry; abandoning a sale is not.
    assert!(!OperationV1::GetReceipt(TransactionRequestV1 {
        transaction_id: "t".to_owned()
    })
    .is_command());
    assert!(
        OperationV1::CancelTransaction(edge_protocol::CancelTransactionRequestV1::default())
            .is_command()
    );

    for (error, expected) in [
        (
            AppError::ReceiptNotFound {
                transaction_id: "t".to_owned(),
            },
            error_codes::RECEIPT_NOT_FOUND,
        ),
        (
            AppError::ReceiptAlreadyPrinted {
                transaction_id: "t".to_owned(),
            },
            error_codes::RECEIPT_ALREADY_PRINTED,
        ),
        (
            AppError::ReceiptOutcomeUnknown {
                transaction_id: "t".to_owned(),
            },
            error_codes::RECEIPT_OUTCOME_UNKNOWN,
        ),
    ] {
        assert_eq!(error.kind(), expected);
    }
}

/// Closing the day answers the day and its pending receipts in one result.
#[test]
fn the_day_close_result_carries_its_receipt_attention() {
    let wire = serde_json::to_value(edge_protocol::ResultV1::BusinessDayClose(
        edge_protocol::BusinessDayCloseV1 {
            protocol_version: edge_protocol::PROTOCOL_VERSION,
            day: edge_protocol::BusinessDayV1 {
                business_day_id: "d".to_owned(),
                tenant_id: "t".to_owned(),
                store_id: "s".to_owned(),
                status: "CLOSED".to_owned(),
                opened_at: "2026-10-08T15:00:00Z".to_owned(),
                closed_at: Some("2026-10-08T23:00:00Z".to_owned()),
                version: 2,
            },
            receipt_attention: edge_protocol::ReceiptAttentionV1 {
                pending: 0,
                failed: 1,
                unknown: 2,
            },
        },
    ))
    .unwrap();
    assert_eq!(wire["result"], "BUSINESS_DAY_CLOSE");
    assert_eq!(wire["payload"]["day"]["status"], "CLOSED");
    assert_eq!(wire["payload"]["receipt_attention"]["failed"], 1);
    assert_eq!(wire["payload"]["receipt_attention"]["unknown"], 2);
    // `PRINTED` is not an attention item, so there is no field for it.
    assert!(wire["payload"]["receipt_attention"]["printed"].is_null());
}

/// `RECEIPT_COMPLETED` is gone from the wire, and a client cannot ask for it.
#[test]
fn the_sale_lifecycle_has_no_receipt_state() {
    for status in [
        "OPEN",
        "TOTALIZED",
        "PAYMENT_IN_PROGRESS",
        "COMMITTED",
        "FINALIZED",
        "CANCELLED",
    ] {
        assert!(
            TransactionStatus::parse(status).is_ok(),
            "{status} is part of the lifecycle"
        );
    }
    assert!(
        TransactionStatus::parse("RECEIPT_COMPLETED").is_err(),
        "a receipt is an obligation, not a state of the sale (ADR 043)"
    );
}

/// A cancellation payload cannot carry money, a line or a tender.
#[test]
fn the_cancel_payload_can_only_say_why() {
    let wire = serde_json::to_value(edge_protocol::CancelTransactionRequestV1 {
        reason_code: Some("CUSTOMER_LEFT".to_owned()),
    })
    .unwrap();
    let keys: Vec<&String> = wire
        .as_object()
        .expect("an object")
        .keys()
        .collect::<Vec<_>>();
    assert_eq!(keys, vec!["reason_code"]);
}
