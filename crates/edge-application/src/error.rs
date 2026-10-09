//! Typed application errors.
//!
//! Cross-aggregate invariants get their own variant so that clients (CLI, UI,
//! future daemon) can react programmatically instead of matching on strings.

use edge_cash::CashError;
use edge_catalog::CatalogError;
use edge_domain::prelude::DomainError;
use edge_inventory::InventoryError;
use edge_payments::PaymentError;
use edge_refunds::RefundError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Domain(#[from] DomainError),

    #[error(transparent)]
    Catalog(#[from] CatalogError),

    #[error(transparent)]
    Payment(#[from] PaymentError),

    #[error(transparent)]
    Cash(#[from] CashError),

    #[error(transparent)]
    Inventory(#[from] InventoryError),

    #[error(transparent)]
    Refund(#[from] RefundError),

    #[error("the session cannot close yet: {}", blockers.join(", "))]
    SessionCloseBlocked { blockers: Vec<String> },

    #[error("the business day cannot close yet: {}", blockers.join(", "))]
    BusinessDayCloseBlocked { blockers: Vec<String> },

    #[error("storage failure: {0}")]
    Storage(String),

    #[error("stale write detected for {aggregate} {id}")]
    ConcurrentModification { aggregate: &'static str, id: String },

    #[error("serialization failure: {0}")]
    Serialization(String),

    #[error("edge device is not initialized; run `init-demo` first")]
    DeviceNotInitialized,

    #[error("no transaction `{transaction_id}`")]
    TransactionNotFound { transaction_id: String },

    #[error("no refund `{refund_id}`")]
    RefundNotFound { refund_id: String },

    #[error("business day {business_day_id} is already open for this store")]
    BusinessDayAlreadyOpen { business_day_id: String },

    #[error("no open business day for this store")]
    NoOpenBusinessDay,

    #[error("business day is {status}, not OPEN")]
    BusinessDayNotOpen { status: &'static str },

    #[error("business day still has {count} open register session(s)")]
    BusinessDayHasOpenSessions { count: usize },

    #[error("register {register_id} already has an open session ({session_id})")]
    RegisterSessionAlreadyOpen {
        register_id: String,
        session_id: String,
    },

    #[error("no open register session")]
    NoOpenRegisterSession,

    #[error("{count} open register sessions found; the active one is ambiguous")]
    AmbiguousOpenSession { count: usize },

    #[error("register session is {status}, not OPEN")]
    RegisterSessionNotOpen { status: &'static str },

    #[error("session still has an active transaction ({transaction_id})")]
    SessionHasActiveTransaction { transaction_id: String },

    #[error("a transaction is already active ({transaction_id})")]
    TransactionAlreadyActive { transaction_id: String },

    #[error("no active transaction")]
    NoActiveTransaction,
}

impl AppError {
    /// Stable machine-readable discriminator for clients.
    pub fn kind(&self) -> &'static str {
        match self {
            // A refused state-machine transition is its own stable code: it is
            // the most common thing a client has to react to.
            AppError::Domain(DomainError::InvalidTransition { .. }) => "INVALID_TRANSACTION_STATE",
            // Naming a line that is not there is a client mistake, not a
            // broken invariant, so it gets its own code.
            AppError::Domain(DomainError::LineNotFound { .. })
            | AppError::Domain(DomainError::AdjustmentTargetMissing { .. }) => "LINE_NOT_FOUND",
            // A frozen basket is not a broken invariant: the client has to
            // pay or reverse first (ADR 019).
            AppError::Domain(DomainError::BasketLockedByPayment { .. }) => {
                "BASKET_LOCKED_BY_PAYMENT"
            }
            AppError::Domain(DomainError::UnresolvedPayment) => "PAYMENT_REQUIRES_RECONCILIATION",
            AppError::Domain(_) => "DOMAIN_RULE_VIOLATION",
            // The catalog context publishes its own codes.
            AppError::Catalog(error) => error.code(),
            // The payment context publishes its own codes.
            AppError::Payment(error) => error.code(),
            // The cash context publishes its own codes.
            AppError::Cash(error) => error.code(),
            // The inventory context publishes its own codes.
            AppError::Inventory(error) => error.code(),
            // The refund context publishes its own codes.
            AppError::Refund(error) => error.code(),
            AppError::SessionCloseBlocked { .. } => "SESSION_CLOSE_BLOCKED",
            AppError::BusinessDayCloseBlocked { .. } => "BUSINESS_DAY_CLOSE_BLOCKED",
            AppError::Storage(_) => "STORAGE_FAILURE",
            AppError::ConcurrentModification { .. } => "CONCURRENT_MODIFICATION",
            AppError::Serialization(_) => "SERIALIZATION_FAILURE",
            AppError::DeviceNotInitialized => "DEVICE_NOT_INITIALIZED",
            AppError::TransactionNotFound { .. } => "TRANSACTION_NOT_FOUND",
            AppError::RefundNotFound { .. } => "REFUND_NOT_FOUND",
            AppError::BusinessDayAlreadyOpen { .. } => "BUSINESS_DAY_ALREADY_OPEN",
            AppError::NoOpenBusinessDay => "NO_OPEN_BUSINESS_DAY",
            AppError::BusinessDayNotOpen { .. } => "BUSINESS_DAY_NOT_OPEN",
            AppError::BusinessDayHasOpenSessions { .. } => "BUSINESS_DAY_HAS_OPEN_SESSIONS",
            AppError::RegisterSessionAlreadyOpen { .. } => "REGISTER_SESSION_ALREADY_OPEN",
            AppError::NoOpenRegisterSession => "NO_OPEN_REGISTER_SESSION",
            AppError::AmbiguousOpenSession { .. } => "AMBIGUOUS_OPEN_SESSION",
            AppError::RegisterSessionNotOpen { .. } => "REGISTER_SESSION_NOT_OPEN",
            AppError::SessionHasActiveTransaction { .. } => "SESSION_HAS_ACTIVE_TRANSACTION",
            AppError::TransactionAlreadyActive { .. } => "TRANSACTION_ALREADY_ACTIVE",
            AppError::NoActiveTransaction => "NO_ACTIVE_TRANSACTION",
        }
    }
}
