//! Pins the error-code contract.
//!
//! Clients branch on these strings, so a rename in the core must not silently
//! change what a UI sees. `edge-protocol` cannot depend on `edge-application`,
//! which is why the equality is asserted here instead of in a type.
#![allow(clippy::unwrap_used, clippy::panic)]

use edge_application::AppError;
use edge_domain::prelude::{DomainError, MoneyError};
use edge_protocol::error_codes;

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
