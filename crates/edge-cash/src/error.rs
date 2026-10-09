//! Cash errors, with stable machine-readable codes.

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CashError {
    #[error("a cash amount must be positive, got {minor}")]
    NonPositiveAmount { minor: i64 },

    #[error("{movement_type} must move cash {expected}, not {actual}")]
    WrongDirection {
        movement_type: &'static str,
        expected: &'static str,
        actual: &'static str,
    },

    /// `amount = tendered − change` is the ledger's statement of what stayed
    /// in the drawer (ADR 021).
    #[error("a cash sale of {amount_minor} does not balance {tendered_minor} tendered against {change_minor} change")]
    UnbalancedCashSale {
        amount_minor: i64,
        tendered_minor: i64,
        change_minor: i64,
    },

    #[error("a {field} is required and must not be blank")]
    MissingReason { field: &'static str },

    #[error("safe drop `{safe_drop_id}` cannot go from `{from}` to `{attempted}`")]
    InvalidSafeDropTransition {
        safe_drop_id: String,
        from: &'static str,
        attempted: &'static str,
    },

    /// The one authorisation rule the domain does enforce, because it is a
    /// property of the record and not of a role (ADR 023).
    #[error("a safe drop cannot be approved by the operator who requested it")]
    SelfApproval,

    #[error("safe drop `{safe_drop_id}` not found")]
    SafeDropNotFound { safe_drop_id: String },

    #[error("the session must declare its counted cash before it can close")]
    DeclarationRequired,

    #[error("this close attempt already has a declaration")]
    AlreadyDeclared,

    #[error("cash record is invalid: {field} {reason}")]
    InvalidRecord { field: &'static str, reason: String },

    #[error("unknown {kind} value `{value}`")]
    UnknownEnumValue { kind: &'static str, value: String },

    #[error("{0}")]
    Money(#[from] edge_domain::prelude::MoneyError),
}

impl CashError {
    /// Stable code. Part of the protocol contract.
    pub const fn code(&self) -> &'static str {
        match self {
            CashError::NonPositiveAmount { .. } => "CASH_INVALID_AMOUNT",
            CashError::WrongDirection { .. } | CashError::UnbalancedCashSale { .. } => {
                "CASH_INVALID_MOVEMENT"
            }
            CashError::MissingReason { .. } => "CASH_REASON_REQUIRED",
            CashError::InvalidSafeDropTransition { .. } => "SAFE_DROP_INVALID_STATE",
            CashError::SelfApproval => "SAFE_DROP_SELF_APPROVAL",
            CashError::SafeDropNotFound { .. } => "SAFE_DROP_NOT_FOUND",
            CashError::DeclarationRequired => "SESSION_DECLARATION_REQUIRED",
            CashError::AlreadyDeclared => "SESSION_ALREADY_DECLARED",
            CashError::InvalidRecord { .. } | CashError::UnknownEnumValue { .. } => {
                "CASH_INVALID_RECORD"
            }
            CashError::Money(_) => "DOMAIN_RULE_VIOLATION",
        }
    }
}
