//! Pins the error-code contract.
//!
//! Clients branch on these strings, so a rename in the core must not silently
//! change what a UI sees. `edge-protocol` cannot depend on `edge-application`,
//! which is why the equality is asserted here instead of in a type.
#![allow(clippy::unwrap_used, clippy::panic)]

use edge_application::AppError;
use edge_catalog::CatalogError;
use edge_domain::prelude::{DomainError, MoneyError};
use edge_protocol::{
    error_codes, AddItemRequestV1, ChangeQuantityRequestV1, OperationV1, ProductSelectorV1,
    RemoveItemRequestV1,
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
