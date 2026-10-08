//! Outbox: durable, at-least-once hand-off to PI.COMMERCE Server.
//!
//! Events are written in the **same SQLite transaction** as the aggregate they
//! describe, so a finalized sale can never exist without its outbox row.
//! `PENDING` rows are never deleted (enforced by a SQLite trigger); a delivered
//! row transitions to `SENT` and is kept for reconciliation.
//!
//! `event_id` is the idempotency key the server must deduplicate on.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::DomainError;
use crate::ids::{BusinessDayId, EventId, RegisterId, StoreId, TenantId};
use crate::time::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OutboxStatus {
    Pending,
    Sending,
    Sent,
}

impl OutboxStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            OutboxStatus::Pending => "PENDING",
            OutboxStatus::Sending => "SENDING",
            OutboxStatus::Sent => "SENT",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "PENDING" => Ok(OutboxStatus::Pending),
            "SENDING" => Ok(OutboxStatus::Sending),
            "SENT" => Ok(OutboxStatus::Sent),
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
    RegisterSessionClosed,
    BusinessDayClosed,
}

impl OutboxEventType {
    pub const fn as_str(self) -> &'static str {
        match self {
            OutboxEventType::SaleFinalized => "SALE_FINALIZED",
            OutboxEventType::RegisterSessionClosed => "REGISTER_SESSION_CLOSED",
            OutboxEventType::BusinessDayClosed => "BUSINESS_DAY_CLOSED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "SALE_FINALIZED" => Ok(OutboxEventType::SaleFinalized),
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

impl OutboxEvent {
    /// Build a new `PENDING` event, ready for the (not yet implemented)
    /// dispatcher.
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
