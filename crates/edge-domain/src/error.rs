//! Typed domain errors. No strings-as-errors in the core.

use crate::ids::IdError;
use crate::money::MoneyError;
use crate::time::TimeError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    #[error(transparent)]
    Money(#[from] MoneyError),
    #[error(transparent)]
    Id(#[from] IdError),
    #[error(transparent)]
    Time(#[from] TimeError),

    /// A state machine refused a transition.
    #[error("invalid {aggregate} transition: {from} -> {attempted}")]
    InvalidTransition {
        aggregate: &'static str,
        from: &'static str,
        attempted: &'static str,
    },

    #[error("transaction has no lines")]
    EmptyTransaction,

    #[error("transaction totals are stale; totalize again")]
    StaleTotals,

    #[error("insufficient payment: grand total {required_minor}, committed {paid_minor}")]
    InsufficientPayment {
        required_minor: i64,
        paid_minor: i64,
    },

    #[error("payment amount must be strictly positive")]
    NonPositivePayment,

    #[error("line discount {discount_minor} exceeds line amount {gross_minor}")]
    DiscountExceedsLine {
        discount_minor: i64,
        gross_minor: i64,
    },

    #[error("computed totals are inconsistent with line totals")]
    InconsistentTotals,

    #[error("field `{0}` is required")]
    MissingField(&'static str),

    #[error("line {line_number} is not part of this transaction")]
    LineNotFound { line_number: i64 },

    #[error("a promotion adjustment targets line {line_number}, which does not exist")]
    AdjustmentTargetMissing { line_number: i64 },

    #[error("promotion target `{kind}` is not a valid target")]
    InvalidPromotionTarget { kind: String },

    #[error("the basket of transaction `{transaction_id}` is locked by a payment")]
    BasketLockedByPayment { transaction_id: String },

    #[error("a payment of this sale has an unresolved outcome")]
    UnresolvedPayment,

    #[error("unknown {kind} value `{value}`")]
    UnknownEnumValue { kind: &'static str, value: String },
}
