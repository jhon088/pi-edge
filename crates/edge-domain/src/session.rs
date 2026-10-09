//! RegisterSession aggregate.
//!
//! State machine: `OPEN -> CLOSE_REQUESTED -> CLOSED`.
//!
//! A session is the operational responsibility of one operator for one
//! register over one period. It is not a cash concept — a card-only store has
//! sessions and no drawer — so the cash ledger lives in `edge-cash` (ADR 025)
//! and what the session keeps is *how* it ended and *which* declaration said
//! so.
//!
//! A closed session is never reopened in place. Reopening creates a new
//! session linked to it, so the first close and its variance survive
//! (ADR 023).

use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{BusinessDayId, OperatorId, RegisterId, SessionId};
use crate::money::{Money, MoneyError};
use crate::time::Timestamp;
use uuid::Uuid;

const AGGREGATE: &str = "RegisterSession";

/// How a session ended.
///
/// Stored rather than inferred from a null field: "nobody counted this
/// drawer" and "this drawer counted to zero" must never be the same record
/// (ADR 024).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClosureKind {
    /// An operator counted the drawer and declared it.
    Declared,
    /// A business-day close forced it with no count. The variance is unknown,
    /// not zero.
    Forced,
}

impl ClosureKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            ClosureKind::Declared => "DECLARED",
            ClosureKind::Forced => "FORCED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "DECLARED" => Ok(ClosureKind::Declared),
            "FORCED" => Ok(ClosureKind::Forced),
            other => Err(DomainError::UnknownEnumValue {
                kind: "ClosureKind",
                value: other.to_owned(),
            }),
        }
    }

    pub const fn is_forced(self) -> bool {
        matches!(self, ClosureKind::Forced)
    }
}

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
    /// The float the session opened with. Also written as the ledger's first
    /// `OPENING_FLOAT` movement, so the ledger alone explains the drawer.
    pub opening_cash: Money,
    pub opened_at: Timestamp,
    pub closed_at: Option<Timestamp>,
    /// How it ended. `None` while it is still running.
    pub closure_kind: Option<ClosureKind>,
    /// The declaration that closed it, for the variance an auditor reads.
    pub close_declaration_id: Option<Uuid>,
    /// The session this one continues after a reopen. The earlier session
    /// keeps its own close untouched (ADR 023).
    pub reopened_from_session_id: Option<SessionId>,
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
    pub reopened_from_session_id: Option<SessionId>,
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
            closure_kind: None,
            close_declaration_id: None,
            reopened_from_session_id: new.reopened_from_session_id,
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

    pub fn is_close_requested(&self) -> bool {
        self.0.status == RegisterSessionStatus::CloseRequested
    }

    /// Close the session, recording how it ended and which declaration said so.
    ///
    /// A declared close must come through `CLOSE_REQUESTED`: the declaration is
    /// a reply to a request, and allowing `OPEN -> CLOSED` directly would let a
    /// client skip the blind-close ordering (ADR 022). A forced close may take
    /// a session from either state, because its whole purpose is to end one
    /// nobody came back to (ADR 024).
    pub fn close(
        &mut self,
        kind: ClosureKind,
        declaration_id: Uuid,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        let allowed = match kind {
            ClosureKind::Declared => self.0.status == RegisterSessionStatus::CloseRequested,
            ClosureKind::Forced => self.0.status.is_active(),
        };
        if !allowed {
            return Err(DomainError::InvalidTransition {
                aggregate: AGGREGATE,
                from: self.0.status.as_str(),
                attempted: "CLOSED",
            });
        }
        self.0.status = RegisterSessionStatus::Closed;
        self.0.closed_at = Some(now);
        self.0.closure_kind = Some(kind);
        self.0.close_declaration_id = Some(declaration_id);
        self.0.version += 1;
        Ok(())
    }

    /// Abandon a close request and go back to trading.
    ///
    /// Not a reopen: nothing was closed, so there is no history to preserve.
    pub fn cancel_close_request(&mut self) -> Result<(), DomainError> {
        match self.0.status {
            RegisterSessionStatus::CloseRequested => {
                self.0.status = RegisterSessionStatus::Open;
                self.0.version += 1;
                Ok(())
            }
            other => Err(DomainError::InvalidTransition {
                aggregate: AGGREGATE,
                from: other.as_str(),
                attempted: "OPEN",
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
            reopened_from_session_id: None,
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

    fn now() -> Timestamp {
        Timestamp::parse_rfc3339("2026-10-08T20:00:00Z").unwrap()
    }

    #[test]
    fn a_declared_close_must_come_through_a_close_request() {
        let mut s = session(0).unwrap();
        // Skipping the request would skip the blind-close ordering.
        assert!(s
            .close(ClosureKind::Declared, Uuid::from_u128(9), now())
            .is_err());
        s.request_close().unwrap();
        assert!(s.is_close_requested());
        s.close(ClosureKind::Declared, Uuid::from_u128(9), now())
            .unwrap();
        assert_eq!(s.status(), RegisterSessionStatus::Closed);
        assert_eq!(s.state().closure_kind, Some(ClosureKind::Declared));
        assert_eq!(s.state().close_declaration_id, Some(Uuid::from_u128(9)));
        // And it closes once.
        assert!(s
            .close(ClosureKind::Declared, Uuid::from_u128(9), now())
            .is_err());
    }

    #[test]
    fn a_forced_close_can_end_a_session_from_either_active_state() {
        for prepare_request in [false, true] {
            let mut s = session(0).unwrap();
            if prepare_request {
                s.request_close().unwrap();
            }
            s.close(ClosureKind::Forced, Uuid::from_u128(9), now())
                .unwrap();
            assert_eq!(s.state().closure_kind, Some(ClosureKind::Forced));
            assert!(s.state().closure_kind.unwrap().is_forced());
        }
    }

    #[test]
    fn a_close_request_can_be_abandoned() {
        let mut s = session(0).unwrap();
        s.request_close().unwrap();
        s.cancel_close_request().unwrap();
        assert!(s.is_open());
        // Nothing was closed, so there is nothing to cancel now.
        assert!(s.cancel_close_request().is_err());
    }

    #[test]
    fn a_reopened_session_records_what_it_continues() {
        let previous = SessionId::from_uuid(Uuid::from_u128(77));
        let s = RegisterSession::open(NewRegisterSession {
            session_id: SessionId::from_uuid(Uuid::from_u128(78)),
            business_day_id: BusinessDayId::from_uuid(Uuid::from_u128(1)),
            register_id: RegisterId::new("REG-001").unwrap(),
            operator_id: OperatorId::new("CASHIER-002").unwrap(),
            opening_cash: Money::from_minor(104_000, Currency::Mxn),
            opened_at: now(),
            reopened_from_session_id: Some(previous),
        })
        .unwrap();
        assert_eq!(s.state().reopened_from_session_id, Some(previous));
        assert!(s.is_open());
    }
}
