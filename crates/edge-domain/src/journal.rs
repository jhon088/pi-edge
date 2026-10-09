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
    ItemRemoved,
    LineQuantityChanged,
    TransactionTotalized,
    PaymentStarted,
    PaymentCommitted,
    PaymentUnknown,
    PaymentFailed,
    PaymentReversalRequested,
    PaymentReversed,
    TransactionFinalized,
    TransactionCancelled,
    CashMovementRecorded,
    SafeDropRequested,
    SafeDropApproved,
    SessionCloseRequested,
    SessionCashDeclared,
    SessionClosed,
    SessionReopened,
    BusinessDayCloseRequested,
    BusinessDayClosed,
    BusinessDayForceClosed,
    InventoryBootstrapped,
    InventoryMovementRecorded,
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
            JournalEventType::ItemRemoved => "ITEM_REMOVED",
            JournalEventType::LineQuantityChanged => "LINE_QUANTITY_CHANGED",
            JournalEventType::TransactionTotalized => "TRANSACTION_TOTALIZED",
            JournalEventType::PaymentStarted => "PAYMENT_STARTED",
            JournalEventType::PaymentCommitted => "PAYMENT_COMMITTED",
            JournalEventType::PaymentUnknown => "PAYMENT_UNKNOWN",
            JournalEventType::PaymentFailed => "PAYMENT_FAILED",
            JournalEventType::PaymentReversalRequested => "PAYMENT_REVERSAL_REQUESTED",
            JournalEventType::PaymentReversed => "PAYMENT_REVERSED",
            JournalEventType::TransactionFinalized => "TRANSACTION_FINALIZED",
            JournalEventType::TransactionCancelled => "TRANSACTION_CANCELLED",
            JournalEventType::CashMovementRecorded => "CASH_MOVEMENT_RECORDED",
            JournalEventType::SafeDropRequested => "SAFE_DROP_REQUESTED",
            JournalEventType::SafeDropApproved => "SAFE_DROP_APPROVED",
            JournalEventType::SessionCloseRequested => "SESSION_CLOSE_REQUESTED",
            JournalEventType::SessionCashDeclared => "SESSION_CASH_DECLARED",
            JournalEventType::SessionClosed => "SESSION_CLOSED",
            JournalEventType::SessionReopened => "SESSION_REOPENED",
            JournalEventType::BusinessDayCloseRequested => "BUSINESS_DAY_CLOSE_REQUESTED",
            JournalEventType::BusinessDayClosed => "BUSINESS_DAY_CLOSED",
            JournalEventType::BusinessDayForceClosed => "BUSINESS_DAY_FORCE_CLOSED",
            JournalEventType::InventoryBootstrapped => "INVENTORY_BOOTSTRAPPED",
            JournalEventType::InventoryMovementRecorded => "INVENTORY_MOVEMENT_RECORDED",
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
            "ITEM_REMOVED" => Ok(JournalEventType::ItemRemoved),
            "LINE_QUANTITY_CHANGED" => Ok(JournalEventType::LineQuantityChanged),
            "TRANSACTION_TOTALIZED" => Ok(JournalEventType::TransactionTotalized),
            "PAYMENT_STARTED" => Ok(JournalEventType::PaymentStarted),
            "PAYMENT_COMMITTED" => Ok(JournalEventType::PaymentCommitted),
            "PAYMENT_UNKNOWN" => Ok(JournalEventType::PaymentUnknown),
            "PAYMENT_FAILED" => Ok(JournalEventType::PaymentFailed),
            "PAYMENT_REVERSAL_REQUESTED" => Ok(JournalEventType::PaymentReversalRequested),
            "PAYMENT_REVERSED" => Ok(JournalEventType::PaymentReversed),
            "TRANSACTION_FINALIZED" => Ok(JournalEventType::TransactionFinalized),
            "TRANSACTION_CANCELLED" => Ok(JournalEventType::TransactionCancelled),
            "CASH_MOVEMENT_RECORDED" => Ok(JournalEventType::CashMovementRecorded),
            "SAFE_DROP_REQUESTED" => Ok(JournalEventType::SafeDropRequested),
            "SAFE_DROP_APPROVED" => Ok(JournalEventType::SafeDropApproved),
            "SESSION_CLOSE_REQUESTED" => Ok(JournalEventType::SessionCloseRequested),
            "SESSION_CASH_DECLARED" => Ok(JournalEventType::SessionCashDeclared),
            "SESSION_CLOSED" => Ok(JournalEventType::SessionClosed),
            "SESSION_REOPENED" => Ok(JournalEventType::SessionReopened),
            "BUSINESS_DAY_CLOSE_REQUESTED" => Ok(JournalEventType::BusinessDayCloseRequested),
            "BUSINESS_DAY_CLOSED" => Ok(JournalEventType::BusinessDayClosed),
            "BUSINESS_DAY_FORCE_CLOSED" => Ok(JournalEventType::BusinessDayForceClosed),
            "INVENTORY_BOOTSTRAPPED" => Ok(JournalEventType::InventoryBootstrapped),
            "INVENTORY_MOVEMENT_RECORDED" => Ok(JournalEventType::InventoryMovementRecorded),
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
