//! BusinessDay aggregate.
//!
//! State machine: `OPEN -> CLOSING -> CLOSED` (and `OPEN -> CLOSED` directly).

use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{BusinessDayId, StoreId, TenantId};
use crate::session::ClosureKind;
use crate::time::Timestamp;

const AGGREGATE: &str = "BusinessDay";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BusinessDayStatus {
    Open,
    Closing,
    Closed,
}

impl BusinessDayStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            BusinessDayStatus::Open => "OPEN",
            BusinessDayStatus::Closing => "CLOSING",
            BusinessDayStatus::Closed => "CLOSED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "OPEN" => Ok(BusinessDayStatus::Open),
            "CLOSING" => Ok(BusinessDayStatus::Closing),
            "CLOSED" => Ok(BusinessDayStatus::Closed),
            other => Err(DomainError::UnknownEnumValue {
                kind: "BusinessDayStatus",
                value: other.to_owned(),
            }),
        }
    }

    /// A business day that still owns activity (not terminal).
    pub const fn is_active(self) -> bool {
        matches!(self, BusinessDayStatus::Open | BusinessDayStatus::Closing)
    }
}

/// Durable state of a business day. Read freely; mutate only through
/// [`BusinessDay`] methods so invariants hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessDayState {
    pub business_day_id: BusinessDayId,
    pub tenant_id: TenantId,
    pub store_id: StoreId,
    pub status: BusinessDayStatus,
    pub opened_at: Timestamp,
    pub closed_at: Option<Timestamp>,
    /// How it ended: `DECLARED` when every session closed normally, `FORCED`
    /// when a policy ended it over open sessions (ADR 024). `None` while it is
    /// still trading.
    pub closure_kind: Option<ClosureKind>,
    /// Why it was forced, and who forced it. Mandatory for a forced close: a
    /// forced end of day with no stated reason is a loss of information.
    pub close_reason: Option<String>,
    pub closed_by: Option<String>,
    /// Monotonic aggregate version, used for sync ordering and stale-write detection.
    pub version: i64,
}

#[derive(Debug, Clone)]
pub struct NewBusinessDay {
    pub business_day_id: BusinessDayId,
    pub tenant_id: TenantId,
    pub store_id: StoreId,
    pub opened_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BusinessDay(BusinessDayState);

impl BusinessDay {
    pub fn open(new: NewBusinessDay) -> Self {
        Self(BusinessDayState {
            business_day_id: new.business_day_id,
            tenant_id: new.tenant_id,
            store_id: new.store_id,
            status: BusinessDayStatus::Open,
            opened_at: new.opened_at,
            closed_at: None,
            closure_kind: None,
            close_reason: None,
            closed_by: None,
            version: 1,
        })
    }

    /// Rebuild from persisted state (storage adapters only).
    pub fn rehydrate(state: BusinessDayState) -> Self {
        Self(state)
    }

    pub fn state(&self) -> &BusinessDayState {
        &self.0
    }

    pub fn into_state(self) -> BusinessDayState {
        self.0
    }

    pub fn id(&self) -> BusinessDayId {
        self.0.business_day_id
    }

    pub fn status(&self) -> BusinessDayStatus {
        self.0.status
    }

    pub fn is_open(&self) -> bool {
        self.0.status == BusinessDayStatus::Open
    }

    /// Mark the day as closing: no new sales, pending work may finish.
    pub fn request_close(&mut self) -> Result<(), DomainError> {
        match self.0.status {
            BusinessDayStatus::Open => {
                self.0.status = BusinessDayStatus::Closing;
                self.0.version += 1;
                Ok(())
            }
            other => Err(DomainError::InvalidTransition {
                aggregate: AGGREGATE,
                from: other.as_str(),
                attempted: "CLOSING",
            }),
        }
    }

    pub fn close(&mut self, now: Timestamp) -> Result<(), DomainError> {
        self.close_as(ClosureKind::Declared, None, None, now)
    }

    /// End the day under an explicit policy.
    ///
    /// This does not ignore state: the caller is responsible for having
    /// already forced the sessions it names, which is an auditable flow of its
    /// own (ADR 024). What the day records is that it ended this way, why, and
    /// on whose authority.
    pub fn force_close(
        &mut self,
        reason: String,
        actor: String,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        let reason = reason.trim().to_owned();
        if reason.is_empty() {
            return Err(DomainError::MissingReason { field: "reason" });
        }
        let actor = actor.trim().to_owned();
        if actor.is_empty() {
            return Err(DomainError::MissingReason { field: "actor" });
        }
        self.close_as(ClosureKind::Forced, Some(reason), Some(actor), now)
    }

    fn close_as(
        &mut self,
        kind: ClosureKind,
        reason: Option<String>,
        actor: Option<String>,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        match self.0.status {
            BusinessDayStatus::Open | BusinessDayStatus::Closing => {
                self.0.status = BusinessDayStatus::Closed;
                self.0.closed_at = Some(now);
                self.0.closure_kind = Some(kind);
                self.0.close_reason = reason;
                self.0.closed_by = actor;
                self.0.version += 1;
                Ok(())
            }
            other => Err(DomainError::InvalidTransition {
                aggregate: AGGREGATE,
                from: other.as_str(),
                attempted: "CLOSED",
            }),
        }
    }

    /// Whether the day was ended by policy rather than by closing every till.
    pub fn was_forced(&self) -> bool {
        self.0
            .closure_kind
            .map(ClosureKind::is_forced)
            .unwrap_or(false)
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn day() -> BusinessDay {
        BusinessDay::open(NewBusinessDay {
            business_day_id: BusinessDayId::from_uuid(Uuid::from_u128(1)),
            tenant_id: TenantId::new("TENANT-1").unwrap(),
            store_id: StoreId::new("STORE-1").unwrap(),
            opened_at: Timestamp::parse_rfc3339("2026-10-08T08:00:00Z").unwrap(),
        })
    }

    #[test]
    fn opens_in_open_status_at_version_one() {
        let d = day();
        assert_eq!(d.status(), BusinessDayStatus::Open);
        assert_eq!(d.state().version, 1);
        assert!(d.state().closed_at.is_none());
    }

    #[test]
    fn closes_through_closing() {
        let mut d = day();
        d.request_close().unwrap();
        assert_eq!(d.status(), BusinessDayStatus::Closing);
        let now = Timestamp::parse_rfc3339("2026-10-08T22:00:00Z").unwrap();
        d.close(now).unwrap();
        assert_eq!(d.status(), BusinessDayStatus::Closed);
        assert_eq!(d.state().closed_at, Some(now));
        assert_eq!(d.state().version, 3);
    }

    #[test]
    fn rejects_double_close() {
        let mut d = day();
        let now = Timestamp::parse_rfc3339("2026-10-08T22:00:00Z").unwrap();
        d.close(now).unwrap();
        let err = d.close(now).unwrap_err();
        assert!(matches!(err, DomainError::InvalidTransition { .. }));
    }

    #[test]
    fn rejects_close_request_after_closed() {
        let mut d = day();
        d.close(Timestamp::parse_rfc3339("2026-10-08T22:00:00Z").unwrap())
            .unwrap();
        assert!(d.request_close().is_err());
    }
}
