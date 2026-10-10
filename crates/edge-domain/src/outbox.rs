//! Outbox: durable, at-least-once hand-off to PI.COMMERCE Server.
//!
//! Events are written in the **same SQLite transaction** as the aggregate they
//! describe, so a finalized sale can never exist without its outbox row. A row
//! that is not `SENT` can never be deleted (enforced by a SQLite trigger); a
//! delivered row transitions to `SENT` and is kept for reconciliation.
//!
//! `event_id` is the idempotency key the server must deduplicate on, and it
//! never changes across retries — that is what makes at-least-once safe
//! (ADR 004).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::DomainError;
use crate::ids::{BusinessDayId, EventId, RegisterId, StoreId, TenantId};
use crate::time::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OutboxStatus {
    /// Waiting for its turn, or waiting out a backoff.
    Pending,
    /// Claimed by the dispatcher and in flight.
    ///
    /// Not a resting state: a process that dies mid-flight leaves rows here,
    /// and the sync engine puts them back to `Pending` at startup rather than
    /// leaving them stuck (ADR 041).
    Sending,
    /// The consumer has it.
    Sent,
    /// The consumer rejected it for good. Terminal, visible, never retried
    /// automatically and never deleted: it waits for a person (ADR 041).
    Failed,
}

impl OutboxStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            OutboxStatus::Pending => "PENDING",
            OutboxStatus::Sending => "SENDING",
            OutboxStatus::Sent => "SENT",
            OutboxStatus::Failed => "FAILED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "PENDING" => Ok(OutboxStatus::Pending),
            "SENDING" => Ok(OutboxStatus::Sending),
            "SENT" => Ok(OutboxStatus::Sent),
            "FAILED" => Ok(OutboxStatus::Failed),
            other => Err(DomainError::UnknownEnumValue {
                kind: "OutboxStatus",
                value: other.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OutboxEventType {
    SaleFinalized,
    /// A refund reached `ACCEPTED`: the goods are back and the commercial
    /// return is **irrevocable**, whatever the money does next.
    ///
    /// Published at acceptance rather than at completion, because a refund can
    /// sit in `FINANCIAL_PENDING` indefinitely and the consumer must not learn
    /// about the return only once the money resolves (ADR 041).
    RefundAccepted,
    /// The same refund's financial obligation is settled. Version 2 of the same
    /// aggregate as [`OutboxEventType::RefundAccepted`] (ADR 041).
    RefundCompleted,
    /// A **manual** stock movement. Sale and refund movements are not
    /// published: they are derivable from `SALE_FINALIZED` and
    /// `REFUND_COMPLETED`, and publishing them too would hand the consumer the
    /// same depletion twice (ADR 028).
    InventoryMovementRecorded,
    RegisterSessionClosed,
    BusinessDayClosed,
}

impl OutboxEventType {
    pub const fn as_str(self) -> &'static str {
        match self {
            OutboxEventType::SaleFinalized => "SALE_FINALIZED",
            OutboxEventType::RefundAccepted => "REFUND_ACCEPTED",
            OutboxEventType::RefundCompleted => "REFUND_COMPLETED",
            OutboxEventType::InventoryMovementRecorded => "INVENTORY_MOVEMENT_RECORDED",
            OutboxEventType::RegisterSessionClosed => "REGISTER_SESSION_CLOSED",
            OutboxEventType::BusinessDayClosed => "BUSINESS_DAY_CLOSED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "SALE_FINALIZED" => Ok(OutboxEventType::SaleFinalized),
            "REFUND_ACCEPTED" => Ok(OutboxEventType::RefundAccepted),
            "REFUND_COMPLETED" => Ok(OutboxEventType::RefundCompleted),
            "INVENTORY_MOVEMENT_RECORDED" => Ok(OutboxEventType::InventoryMovementRecorded),
            "REGISTER_SESSION_CLOSED" => Ok(OutboxEventType::RegisterSessionClosed),
            "BUSINESS_DAY_CLOSED" => Ok(OutboxEventType::BusinessDayClosed),
            other => Err(DomainError::UnknownEnumValue {
                kind: "OutboxEventType",
                value: other.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEvent {
    /// Idempotency key for the server.
    pub event_id: EventId,
    pub event_type: OutboxEventType,
    pub aggregate_id: String,
    pub aggregate_version: i64,
    pub tenant_id: TenantId,
    pub store_id: StoreId,
    pub register_id: Option<RegisterId>,
    pub business_day_id: Option<BusinessDayId>,
    pub occurred_at: Timestamp,
    pub payload_json: Value,
    pub status: OutboxStatus,
    pub attempt_count: i64,
    pub next_attempt_at: Option<Timestamp>,
}

/// One durable outbox row, as it is stored.
///
/// `seq` is SQLite's own monotonic sequence and therefore the installation's
/// durable order: it is what forms batches deterministically, and it exists
/// only once the row has been written, which is why it lives here and not on
/// the enqueue-time `OutboxEvent` (ADR 041).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub seq: i64,
    pub event: OutboxEvent,
    /// Why the last attempt did not land. Always present on a `FAILED` row.
    pub last_error_code: Option<String>,
    pub last_error_detail: Option<String>,
}

impl OutboxEvent {
    /// Build a new `PENDING` event, ready for the dispatcher.
    #[allow(clippy::too_many_arguments)]
    pub fn pending(
        event_id: EventId,
        event_type: OutboxEventType,
        aggregate_id: String,
        aggregate_version: i64,
        tenant_id: TenantId,
        store_id: StoreId,
        register_id: Option<RegisterId>,
        business_day_id: Option<BusinessDayId>,
        occurred_at: Timestamp,
        payload_json: Value,
    ) -> Self {
        Self {
            event_id,
            event_type,
            aggregate_id,
            aggregate_version,
            tenant_id,
            store_id,
            register_id,
            business_day_id,
            occurred_at,
            payload_json,
            status: OutboxStatus::Pending,
            attempt_count: 0,
            next_attempt_at: None,
        }
    }
}
