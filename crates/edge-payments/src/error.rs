//! Payment errors, with stable machine-readable codes.
//!
//! Clients branch on [`PaymentError::code`]; the human message may change at
//! any time and no client may parse it.

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PaymentError {
    #[error("payment `{payment_id}` not found")]
    PaymentNotFound { payment_id: String },

    #[error("payment attempt `{attempt_id}` not found")]
    AttemptNotFound { attempt_id: String },

    #[error("payment `{aggregate}` cannot go from `{from}` to `{attempted}`")]
    InvalidTransition {
        aggregate: &'static str,
        from: &'static str,
        attempted: &'static str,
    },

    #[error("payment `{payment_id}` is already resolved as `{status}`")]
    AlreadyResolved {
        payment_id: String,
        status: &'static str,
    },

    /// An attempt reached a provider and we do not know the result. Resolution
    /// is an explicit query, never a timeout and never a retry (ADR 018).
    #[error("the result of payment `{payment_id}` is unknown and must be reconciled")]
    RequiresReconciliation { payment_id: String },

    #[error("reversing payment `{payment_id}` is not allowed from `{status}`")]
    ReversalNotAllowed {
        payment_id: String,
        status: &'static str,
    },

    #[error(
        "{tender} cannot pay {requested_minor} against a remaining balance of {remaining_minor}"
    )]
    ExceedsBalance {
        tender: &'static str,
        requested_minor: i64,
        remaining_minor: i64,
    },

    #[error("a payment amount must be positive, got {minor}")]
    NonPositiveAmount { minor: i64 },

    #[error("the provider declined the payment{}", match reason { Some(r) => format!(": {r}"), None => String::new() })]
    ProviderDeclined { reason: Option<String> },

    #[error("the provider could not be reached or failed: {reason}")]
    ProviderUnavailable { reason: String },

    #[error("there is nothing to pay: the balance is already covered")]
    NothingDue,

    #[error("unknown {kind} value `{value}`")]
    UnknownEnumValue { kind: &'static str, value: String },

    #[error("payment record is invalid: {field} {reason}")]
    InvalidRecord { field: &'static str, reason: String },

    #[error("{0}")]
    Money(#[from] edge_domain::prelude::MoneyError),
}

impl PaymentError {
    /// Stable code. Part of the protocol contract.
    pub const fn code(&self) -> &'static str {
        match self {
            PaymentError::PaymentNotFound { .. } | PaymentError::AttemptNotFound { .. } => {
                "PAYMENT_NOT_FOUND"
            }
            PaymentError::InvalidTransition { .. } => "PAYMENT_INVALID_STATE",
            PaymentError::AlreadyResolved { .. } => "PAYMENT_ALREADY_RESOLVED",
            PaymentError::RequiresReconciliation { .. } => "PAYMENT_REQUIRES_RECONCILIATION",
            PaymentError::ReversalNotAllowed { .. } => "PAYMENT_REVERSAL_NOT_ALLOWED",
            PaymentError::ExceedsBalance { .. } => "PAYMENT_EXCEEDS_BALANCE",
            PaymentError::NonPositiveAmount { .. } | PaymentError::NothingDue => {
                "PAYMENT_INVALID_AMOUNT"
            }
            PaymentError::ProviderDeclined { .. } => "PAYMENT_PROVIDER_DECLINED",
            PaymentError::ProviderUnavailable { .. } => "PAYMENT_PROVIDER_UNAVAILABLE",
            PaymentError::UnknownEnumValue { .. } | PaymentError::InvalidRecord { .. } => {
                "PAYMENT_INVALID_RECORD"
            }
            PaymentError::Money(_) => "DOMAIN_RULE_VIOLATION",
        }
    }
}
