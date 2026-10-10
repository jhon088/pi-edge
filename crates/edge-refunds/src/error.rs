//! Refund failures, each with the stable code clients branch on.

use edge_domain::prelude::{DomainError, MoneyError};
use thiserror::Error;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RefundError {
    #[error("sale `{transaction_id}` is not a refundable document: it is {status}, not FINALIZED")]
    OriginalNotRefundable {
        transaction_id: String,
        status: &'static str,
    },

    /// A refund needs explicit evidence that the original sale is accepted
    /// centrally. PI.Edge never assumes it (ADR 030).
    ///
    /// This one means **there is no authority to ask**: none is configured for
    /// this installation.
    #[error("refunding sale `{transaction_id}` needs central authority over the original sale")]
    AuthorityRequired { transaction_id: String },

    /// There *is* an authority and it could not be reached.
    ///
    /// Distinct from `AuthorityRequired` because the answer to an operator is
    /// different: this one is worth retrying in a minute, and it says the
    /// installation is configured correctly. It is still a refusal — PI.Edge
    /// sells offline and does not refund offline (ADR 042).
    #[error("the refund authority for sale `{transaction_id}` could not be reached: {detail}")]
    AuthorityUnavailable {
        transaction_id: String,
        detail: String,
    },

    /// The authority was reached and said no.
    ///
    /// Retrying changes nothing: the decision was made, and the reason belongs
    /// to the authority (ADR 042).
    #[error("the authority refused to authorize a refund of sale `{transaction_id}`: {reason}")]
    NotAuthorized {
        transaction_id: String,
        reason: String,
    },

    #[error("the original sale has no line {line_number}")]
    LineNotFound { line_number: i64 },

    #[error("the same original line {line_number} cannot be refunded twice in one refund")]
    DuplicateLine { line_number: i64 },

    #[error("a refund has to name at least one line")]
    EmptyRefund,

    #[error("line {line_number} has {remaining} refundable and {requested} was requested")]
    QuantityExceeded {
        line_number: i64,
        remaining: String,
        requested: String,
    },

    /// One non-terminal refund per original sale (ADR 030).
    #[error("sale `{transaction_id}` already has refund `{refund_id}` in progress")]
    RefundInProgress {
        transaction_id: String,
        refund_id: String,
    },

    #[error("refund is {status}, which does not allow {attempted}")]
    InvalidState {
        status: &'static str,
        attempted: &'static str,
    },

    #[error("a refund has to say why")]
    ReasonRequired,

    /// A refund of a sale that was never invoiced has no credit note, and one
    /// of an invoiced sale has to choose (ADR 032).
    #[error("fiscal action {attempted} is not available for an original sale that is {original}")]
    FiscalActionInvalid {
        attempted: &'static str,
        original: &'static str,
    },

    #[error("tender {tender} has {remaining} refundable and {requested} was requested")]
    TenderExceeded {
        tender: &'static str,
        remaining: String,
        requested: String,
    },

    /// The financial refund is unresolved: the only way forward is to ask the
    /// provider about this same attempt (ADR 018, applied to refunds).
    #[error("refund `{refund_id}` has an unresolved financial refund and needs reconciliation")]
    RequiresReconciliation { refund_id: String },

    #[error("invalid refund record: {field} {reason}")]
    InvalidRecord { field: &'static str, reason: String },

    #[error("unknown {kind} value `{value}`")]
    UnknownEnumValue { kind: &'static str, value: String },

    #[error(transparent)]
    Money(#[from] MoneyError),

    #[error(transparent)]
    Domain(#[from] DomainError),
}

impl RefundError {
    /// Stable code. Part of the contract; the message never is.
    pub fn code(&self) -> &'static str {
        match self {
            RefundError::OriginalNotRefundable { .. } => "REFUND_ORIGINAL_NOT_REFUNDABLE",
            RefundError::AuthorityRequired { .. } => "REFUND_AUTHORITY_REQUIRED",
            RefundError::AuthorityUnavailable { .. } => "REFUND_AUTHORITY_UNAVAILABLE",
            RefundError::NotAuthorized { .. } => "REFUND_NOT_AUTHORIZED",
            RefundError::LineNotFound { .. } => "REFUND_LINE_NOT_FOUND",
            RefundError::DuplicateLine { .. } | RefundError::EmptyRefund => "REFUND_INVALID_LINES",
            RefundError::QuantityExceeded { .. } => "REFUND_QUANTITY_EXCEEDED",
            RefundError::RefundInProgress { .. } => "REFUND_IN_PROGRESS",
            RefundError::InvalidState { .. } => "REFUND_INVALID_STATE",
            RefundError::ReasonRequired => "REFUND_REASON_REQUIRED",
            RefundError::FiscalActionInvalid { .. } => "REFUND_FISCAL_ACTION_INVALID",
            RefundError::TenderExceeded { .. } => "REFUND_TENDER_EXCEEDED",
            RefundError::RequiresReconciliation { .. } => "REFUND_REQUIRES_RECONCILIATION",
            RefundError::InvalidRecord { .. } | RefundError::UnknownEnumValue { .. } => {
                "REFUND_INVALID_RECORD"
            }
            RefundError::Money(_) | RefundError::Domain(_) => "DOMAIN_RULE_VIOLATION",
        }
    }
}
