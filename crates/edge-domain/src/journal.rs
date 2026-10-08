//! Journal: append-only structured record of everything that happened on the
//! register. It is the local audit trail and the input for crash forensics.
//!
//! The journal is never mutated or deleted (enforced by SQLite triggers in
//! `edge-storage`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::DomainError;
use crate::ids::{
    BusinessDayId, EventId, OperatorId, RegisterId, SessionId, StoreId, TenantId, TransactionId,
};
use crate::time::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JournalEventType {
    BusinessDayOpened,
    SessionOpened,
    TransactionStarted,
    ItemAdded,
    TransactionTotalized,
    CashPaymentCommitted,
    TransactionFinalized,
    TransactionCancelled,
    SessionClosed,
    BusinessDayClosed,
    CatalogSnapshotImported,
    CatalogSnapshotActivated,
}

impl JournalEventType {
    pub const fn as_str(self) -> &'static str {
        match self {
            JournalEventType::BusinessDayOpened => "BUSINESS_DAY_OPENED",
            JournalEventType::SessionOpened => "SESSION_OPENED",
            JournalEventType::TransactionStarted => "TRANSACTION_STARTED",
            JournalEventType::ItemAdded => "ITEM_ADDED",
            JournalEventType::TransactionTotalized => "TRANSACTION_TOTALIZED",
            JournalEventType::CashPaymentCommitted => "CASH_PAYMENT_COMMITTED",
            JournalEventType::TransactionFinalized => "TRANSACTION_FINALIZED",
            JournalEventType::TransactionCancelled => "TRANSACTION_CANCELLED",
            JournalEventType::SessionClosed => "SESSION_CLOSED",
            JournalEventType::BusinessDayClosed => "BUSINESS_DAY_CLOSED",
            JournalEventType::CatalogSnapshotImported => "CATALOG_SNAPSHOT_IMPORTED",
            JournalEventType::CatalogSnapshotActivated => "CATALOG_SNAPSHOT_ACTIVATED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "BUSINESS_DAY_OPENED" => Ok(JournalEventType::BusinessDayOpened),
            "SESSION_OPENED" => Ok(JournalEventType::SessionOpened),
            "TRANSACTION_STARTED" => Ok(JournalEventType::TransactionStarted),
            "ITEM_ADDED" => Ok(JournalEventType::ItemAdded),
            "TRANSACTION_TOTALIZED" => Ok(JournalEventType::TransactionTotalized),
            "CASH_PAYMENT_COMMITTED" => Ok(JournalEventType::CashPaymentCommitted),
            "TRANSACTION_FINALIZED" => Ok(JournalEventType::TransactionFinalized),
            "TRANSACTION_CANCELLED" => Ok(JournalEventType::TransactionCancelled),
            "SESSION_CLOSED" => Ok(JournalEventType::SessionClosed),
            "BUSINESS_DAY_CLOSED" => Ok(JournalEventType::BusinessDayClosed),
            "CATALOG_SNAPSHOT_IMPORTED" => Ok(JournalEventType::CatalogSnapshotImported),
            "CATALOG_SNAPSHOT_ACTIVATED" => Ok(JournalEventType::CatalogSnapshotActivated),
            other => Err(DomainError::UnknownEnumValue {
                kind: "JournalEventType",
                value: other.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEvent {
    pub event_id: EventId,
    pub event_type: JournalEventType,
    /// Public id of the aggregate the event belongs to, as text.
    pub aggregate_id: String,
    pub tenant_id: TenantId,
    pub store_id: StoreId,
    pub register_id: Option<RegisterId>,
    pub business_day_id: Option<BusinessDayId>,
    pub session_id: Option<SessionId>,
    pub transaction_id: Option<TransactionId>,
    pub operator_id: Option<OperatorId>,
    pub occurred_at: Timestamp,
    pub payload_json: Value,
}
