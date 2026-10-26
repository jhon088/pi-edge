//! PI.Edge domain model.
//!
//! This crate is **pure Rust**. It must never depend on SQLite, HTTP, Tauri,
//! Android, the filesystem or any clock/IO facility. Time and identity are
//! always injected by the caller (see `edge-application` ports).
//!
//! It owns:
//! - value objects ([`money`], [`ids`], [`time`]),
//! - aggregates with explicit state machines ([`business_day`], [`session`],
//!   [`transaction`]),
//! - durable event shapes ([`journal`], [`outbox`], [`receipt`]).

pub mod business_day;
pub mod error;
pub mod ids;
pub mod journal;
pub mod money;
pub mod outbox;
pub mod receipt;
pub mod session;
pub mod time;
pub mod transaction;

pub mod prelude {
    //! Convenience re-export for the application and adapter layers.
    pub use crate::business_day::{
        BusinessDay, BusinessDayState, BusinessDayStatus, NewBusinessDay,
    };
    pub use crate::error::DomainError;
    pub use crate::ids::{
        BusinessDayId, CatalogSnapshotId, CategoryRef, ChannelRef, EventId, IdError, LineId,
        OperatorId, PaymentId, PriceBookId, ProductRef, PromotionId, RefundId, RegisterId,
        SelectionGroupId, SelectionOptionId, SessionId, StoreId, TenantId, TransactionId,
    };
    pub use crate::journal::{JournalEvent, JournalEventType};
    pub use crate::money::{
        allocate_proportionally, Currency, Money, MoneyError, Percentage, PriceMode, Quantity,
        TaxRate, TaxedAmount,
    };
    pub use crate::outbox::{OutboxEntry, OutboxEvent, OutboxEventType, OutboxStatus};
    pub use crate::receipt::{
        ReceiptAttention, ReceiptAttentionScope, ReceiptOutcome, ReceiptStatus, TransactionReceipt,
    };
    pub use crate::session::{
        ClosureKind, NewRegisterSession, RegisterSession, RegisterSessionState,
        RegisterSessionStatus,
    };
    pub use crate::time::{TimeError, Timestamp};
    pub use crate::transaction::{
        AppliedPromotion, LineConfiguration, LineDiscount, NewLineInput, NewRetailTransaction,
        PaymentSummary, PricingCapture, PromotionOutcome, PromotionTarget, RetailTransaction,
        RetailTransactionState, Totals, TransactionLine, TransactionSelection, TransactionStatus,
    };
}

/// Version of the domain model, propagated to clients for diagnostics.
pub const DOMAIN_VERSION: &str = env!("CARGO_PKG_VERSION");
