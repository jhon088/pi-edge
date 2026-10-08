//! RegisterSession aggregate.
//!
//! State machine: `OPEN -> CLOSE_REQUESTED -> CLOSED` (and `OPEN -> CLOSED`).

use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{BusinessDayId, OperatorId, RegisterId, SessionId};
use crate::money::{Money, MoneyError};
use crate::time::Timestamp;

const AGGREGATE: &str = "RegisterSession";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RegisterSessionStatus {
    Open,
    CloseRequested,
    Closed,
}

impl RegisterSessionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            RegisterSessionStatus::Open => "OPEN",
            RegisterSessionStatus::CloseRequested => "CLOSE_REQUESTED",
            RegisterSessionStatus::Closed => "CLOSED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "OPEN" => Ok(RegisterSessionStatus::Open),
            "CLOSE_REQUESTED" => Ok(RegisterSessionStatus::CloseRequested),
            "CLOSED" => Ok(RegisterSessionStatus::Closed),
            other => Err(DomainError::UnknownEnumValue {
                kind: "RegisterSessionStatus",
                value: other.to_owned(),
            }),
        }
    }

    pub const fn is_active(self) -> bool {
        matches!(
            self,
            RegisterSessionStatus::Open | RegisterSessionStatus::CloseRequested
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterSessionState {
    pub session_id: SessionId,
    pub business_day_id: BusinessDayId,
    pub register_id: RegisterId,
    pub operator_id: OperatorId,
    pub status: RegisterSessionStatus,
    pub opening_cash: Money,
    pub opened_at: Timestamp,
    pub closed_at: Option<Timestamp>,
    pub version: i64,
}

#[derive(Debug, Clone)]
pub struct NewRegisterSession {
    pub session_id: SessionId,
    pub business_day_id: BusinessDayId,
    pub register_id: RegisterId,
    pub operator_id: OperatorId,
    pub opening_cash: Money,
    pub opened_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RegisterSession(RegisterSessionState);

impl RegisterSession {
    pub fn open(new: NewRegisterSession) -> Result<Self, DomainError> {
        if new.opening_cash.is_negative() {
            return Err(DomainError::Money(MoneyError::NegativeAmount(
                new.opening_cash.minor(),
            )));
        }
        Ok(Self(RegisterSessionState {
            session_id: new.session_id,
            business_day_id: new.business_day_id,
            register_id: new.register_id,
            operator_id: new.operator_id,
            status: RegisterSessionStatus::Open,
            opening_cash: new.opening_cash,
            opened_at: new.opened_at,
            closed_at: None,
            version: 1,
        }))
    }

    /// Rebuild from persisted state (storage adapters only).
    pub fn rehydrate(state: RegisterSessionState) -> Self {
        Self(state)
    }

    pub fn state(&self) -> &RegisterSessionState {
        &self.0
    }

    pub fn into_state(self) -> RegisterSessionState {
        self.0
    }

    pub fn id(&self) -> SessionId {
        self.0.session_id
    }

    pub fn status(&self) -> RegisterSessionStatus {
        self.0.status
    }

    pub fn is_open(&self) -> bool {
        self.0.status == RegisterSessionStatus::Open
    }

    pub fn request_close(&mut self) -> Result<(), DomainError> {
        match self.0.status {
            RegisterSessionStatus::Open => {
                self.0.status = RegisterSessionStatus::CloseRequested;
                self.0.version += 1;
                Ok(())
            }
            other => Err(DomainError::InvalidTransition {
                aggregate: AGGREGATE,
                from: other.as_str(),
                attempted: "CLOSE_REQUESTED",
            }),
        }
    }

    pub fn close(&mut self, now: Timestamp) -> Result<(), DomainError> {
        match self.0.status {
            RegisterSessionStatus::Open | RegisterSessionStatus::CloseRequested => {
                self.0.status = RegisterSessionStatus::Closed;
                self.0.closed_at = Some(now);
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
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::Currency;
    use uuid::Uuid;

    fn session(opening_cash_minor: i64) -> Result<RegisterSession, DomainError> {
        RegisterSession::open(NewRegisterSession {
            session_id: SessionId::from_uuid(Uuid::from_u128(2)),
            business_day_id: BusinessDayId::from_uuid(Uuid::from_u128(1)),
            register_id: RegisterId::new("REG-001").unwrap(),
            operator_id: OperatorId::new("CASHIER-001").unwrap(),
            opening_cash: Money::from_minor(opening_cash_minor, Currency::Mxn),
            opened_at: Timestamp::parse_rfc3339("2026-10-08T08:05:00Z").unwrap(),
        })
    }

    #[test]
    fn opens_with_opening_float() {
        let s = session(100_000).unwrap();
        assert!(s.is_open());
        assert_eq!(s.state().opening_cash.minor(), 100_000);
        assert_eq!(s.state().version, 1);
    }

    #[test]
    fn rejects_negative_opening_cash() {
        assert!(session(-1).is_err());
    }

    #[test]
    fn closes_once() {
        let mut s = session(0).unwrap();
        let now = Timestamp::parse_rfc3339("2026-10-08T20:00:00Z").unwrap();
        s.close(now).unwrap();
        assert_eq!(s.status(), RegisterSessionStatus::Closed);
        assert!(s.close(now).is_err());
    }
}
