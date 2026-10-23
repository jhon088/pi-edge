//! Typed application errors.
//!
//! Cross-aggregate invariants get their own variant so that clients (CLI, UI,
//! future daemon) can react programmatically instead of matching on strings.

use edge_cash::CashError;
use edge_catalog::CatalogError;
use edge_domain::prelude::DomainError;
use edge_hardware::HardwareError;
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

    /// A device failed. Never a domain failure: the money and the goods are
    /// already where the ledger says they are (ADR 005, ADR 037).
    #[error(transparent)]
    Hardware(#[from] HardwareError),

    /// Sync could not happen. **Never a commercial failure**: an absent,
    /// unreachable or unreadable server changes nothing about what this
    /// installation may sell (ADR 040).
    #[error(transparent)]
    Sync(#[from] edge_sync::SyncError),

    /// A scan arrived with no sale to add it to. PI.Edge does not open one on
    /// its own (ADR 036).
    #[error("a scan needs an active sale; none is open")]
    ScanWithoutTransaction,

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

    /// No receipt obligation for this sale. A finalized sale always has one;
    /// a sale that never finalized never did.
    #[error("sale `{transaction_id}` has no receipt on record")]
    ReceiptNotFound { transaction_id: String },

    /// The first receipt already came out. A second copy is a reprint and has
    /// to say so (ADR 043).
    #[error("sale `{transaction_id}` already has its receipt")]
    ReceiptAlreadyPrinted { transaction_id: String },

    /// Paper may already have come out of the printer. PI.Edge will not print
    /// a first receipt again on a guess; an operator decides, with an explicit
    /// reprint (ADR 043).
    #[error(
        "whether the receipt for sale `{transaction_id}` printed is unknown; \
         print a reprint explicitly if it did not"
    )]
    ReceiptOutcomeUnknown { transaction_id: String },

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
            // The hardware context publishes its own codes.
            AppError::Hardware(error) => error.code(),
            AppError::Sync(error) => error.code(),
            AppError::ScanWithoutTransaction => "SCAN_NO_ACTIVE_TRANSACTION",
            AppError::SessionCloseBlocked { .. } => "SESSION_CLOSE_BLOCKED",
            AppError::BusinessDayCloseBlocked { .. } => "BUSINESS_DAY_CLOSE_BLOCKED",
            AppError::Storage(_) => "STORAGE_FAILURE",
            AppError::ConcurrentModification { .. } => "CONCURRENT_MODIFICATION",
            AppError::Serialization(_) => "SERIALIZATION_FAILURE",
            AppError::DeviceNotInitialized => "DEVICE_NOT_INITIALIZED",
            AppError::ReceiptNotFound { .. } => "RECEIPT_NOT_FOUND",
            AppError::ReceiptAlreadyPrinted { .. } => "RECEIPT_ALREADY_PRINTED",
            AppError::ReceiptOutcomeUnknown { .. } => "RECEIPT_OUTCOME_UNKNOWN",
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
