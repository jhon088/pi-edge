//! The append-only drawer ledger (ADR 021).
//!
//! A movement is a fact: it is written once and never updated or deleted. A
//! mistake is corrected by a new, explicit, compensating movement, so the
//! history of a drawer always reads as what actually happened rather than as
//! what someone later wished had happened.

use edge_domain::prelude::{BusinessDayId, Money, OperatorId, RegisterId, SessionId, Timestamp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CashError;

/// Which way the money went.
///
/// Explicit rather than the sign of a `Money`: amounts stay non-negative
/// everywhere in PI.Edge, and reading a drawer's history must not depend on
/// noticing a minus sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Direction {
    In,
    Out,
}

impl Direction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Direction::In => "IN",
            Direction::Out => "OUT",
        }
    }

    pub fn parse(value: &str) -> Result<Self, CashError> {
        match value {
            "IN" => Ok(Direction::In),
            "OUT" => Ok(Direction::Out),
            other => Err(CashError::UnknownEnumValue {
                kind: "Direction",
                value: other.to_owned(),
            }),
        }
    }
}

/// Why the money moved.
///
/// Each type arrives with its own use case: a type nothing can produce, with no
/// operation and no authorisation policy behind it, is a promise rather than a
/// model. `CASH_REFUND` joined in milestone 9, when commercial refunds gave it
/// one. A manual cash adjustment is still absent, for the same reason as
/// before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MovementType {
    /// The float the session opened with.
    OpeningFloat,
    /// The net cash a committed cash payment left in the drawer.
    CashSale,
    /// Money put in for an operational reason.
    CashIn,
    /// Money taken out for an operational reason.
    CashOut,
    /// Money moved to the safe, once a supervisor approved it (ADR 023).
    SafeDrop,
    /// Cash handed back for a commercial refund. Exists because refunds exist
    /// (milestone 9); it is not a reversal of a `CASH_SALE` (ADR 034).
    CashRefund,
}

impl MovementType {
    pub const fn as_str(self) -> &'static str {
        match self {
            MovementType::OpeningFloat => "OPENING_FLOAT",
            MovementType::CashSale => "CASH_SALE",
            MovementType::CashIn => "CASH_IN",
            MovementType::CashOut => "CASH_OUT",
            MovementType::SafeDrop => "SAFE_DROP",
            MovementType::CashRefund => "CASH_REFUND",
        }
    }

    pub fn parse(value: &str) -> Result<Self, CashError> {
        match value {
            "OPENING_FLOAT" => Ok(MovementType::OpeningFloat),
            "CASH_SALE" => Ok(MovementType::CashSale),
            "CASH_IN" => Ok(MovementType::CashIn),
            "CASH_OUT" => Ok(MovementType::CashOut),
            "SAFE_DROP" => Ok(MovementType::SafeDrop),
            "CASH_REFUND" => Ok(MovementType::CashRefund),
            other => Err(CashError::UnknownEnumValue {
                kind: "MovementType",
                value: other.to_owned(),
            }),
        }
    }

    /// The direction this type moves. Every type has exactly one, and the
    /// pairing is checked on construction and again by a database `CHECK`.
    pub const fn direction(self) -> Direction {
        match self {
            MovementType::OpeningFloat | MovementType::CashSale | MovementType::CashIn => {
                Direction::In
            }
            MovementType::CashOut | MovementType::SafeDrop | MovementType::CashRefund => {
                Direction::Out
            }
        }
    }

    /// Whether this type is created by an operator rather than by the runtime,
    /// and therefore has to say why.
    pub const fn requires_reason(self) -> bool {
        matches!(self, MovementType::CashIn | MovementType::CashOut)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CashMovement {
    pub movement_id: Uuid,
    pub business_day_id: BusinessDayId,
    pub session_id: SessionId,
    pub register_id: RegisterId,
    /// Who caused the movement. For a sale it is the operator of the session.
    pub operator_id: OperatorId,
    pub movement_type: MovementType,
    pub direction: Direction,
    /// What the drawer gained or lost. Never negative.
    pub amount: Money,
    /// `CASH_SALE` only: what the customer handed over.
    pub tendered: Option<Money>,
    /// `CASH_SALE` only: what went back to the customer.
    pub change: Option<Money>,
    /// What caused it, when something else owns that fact: a `payment_id` for
    /// a sale, a `safe_drop_id` for a drop. Also the idempotency key.
    pub source_ref: Option<String>,
    pub reason_code: Option<String>,
    /// The approval that authorised it, where one was required.
    pub approval_id: Option<Uuid>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct NewCashMovement {
    pub movement_id: Uuid,
    pub business_day_id: BusinessDayId,
    pub session_id: SessionId,
    pub register_id: RegisterId,
    pub operator_id: OperatorId,
    pub movement_type: MovementType,
    pub direction: Direction,
    pub amount: Money,
    pub tendered: Option<Money>,
    pub change: Option<Money>,
    pub source_ref: Option<String>,
    pub reason_code: Option<String>,
    pub approval_id: Option<Uuid>,
    pub now: Timestamp,
}

impl CashMovement {
    pub fn record(new: NewCashMovement) -> Result<Self, CashError> {
        if !new.amount.is_positive() {
            return Err(CashError::NonPositiveAmount {
                minor: new.amount.minor(),
            });
        }
        let expected = new.movement_type.direction();
        if expected != new.direction {
            return Err(CashError::WrongDirection {
                movement_type: new.movement_type.as_str(),
                expected: expected.as_str(),
                actual: new.direction.as_str(),
            });
        }
        if new.movement_type.requires_reason() {
            let blank = new
                .reason_code
                .as_deref()
                .map(|reason| reason.trim().is_empty())
                .unwrap_or(true);
            if blank {
                return Err(CashError::MissingReason {
                    field: "reason_code",
                });
            }
        }
        // The cash-sale audit has to agree with what stayed in the drawer, or
        // the ledger and the payment would tell two different stories.
        match (new.movement_type, new.tendered, new.change) {
            (MovementType::CashSale, Some(tendered), Some(change)) => {
                let stayed = tendered.checked_sub(change)?;
                if stayed != new.amount {
                    return Err(CashError::UnbalancedCashSale {
                        amount_minor: new.amount.minor(),
                        tendered_minor: tendered.minor(),
                        change_minor: change.minor(),
                    });
                }
            }
            (MovementType::CashSale, _, _) => {
                return Err(CashError::InvalidRecord {
                    field: "tendered",
                    reason: "a cash sale must record what was tendered and returned".to_owned(),
                })
            }
            (_, None, None) => {}
            _ => {
                return Err(CashError::InvalidRecord {
                    field: "tendered",
                    reason: "only a cash sale carries tendered and change".to_owned(),
                })
            }
        }
        Ok(Self {
            movement_id: new.movement_id,
            business_day_id: new.business_day_id,
            session_id: new.session_id,
            register_id: new.register_id,
            operator_id: new.operator_id,
            movement_type: new.movement_type,
            direction: new.direction,
            amount: new.amount,
            tendered: new.tendered,
            change: new.change,
            source_ref: new.source_ref,
            reason_code: new.reason_code,
            approval_id: new.approval_id,
            created_at: new.now,
        })
    }

    /// Rebuild from storage without re-running the construction rules.
    pub fn rehydrate(movement: Self) -> Self {
        movement
    }

    /// Signed contribution to expected cash, for the arithmetic in
    /// [`crate::position`]. Not exposed as the movement's amount, which stays
    /// non-negative.
    pub fn signed_minor(&self) -> i64 {
        match self.direction {
            Direction::In => self.amount.minor(),
            Direction::Out => -self.amount.minor(),
        }
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use edge_domain::prelude::Currency;

    const MXN: Currency = Currency::Mxn;

    fn money(minor: i64) -> Money {
        Money::from_minor(minor, MXN)
    }

    fn ts() -> Timestamp {
        Timestamp::parse_rfc3339("2026-10-08T15:00:00Z").unwrap()
    }

    fn new(movement_type: MovementType, direction: Direction, minor: i64) -> NewCashMovement {
        NewCashMovement {
            movement_id: Uuid::from_u128(1),
            business_day_id: BusinessDayId::from_uuid(Uuid::from_u128(2)),
            session_id: SessionId::from_uuid(Uuid::from_u128(3)),
            register_id: RegisterId::new("REG-001").unwrap(),
            operator_id: OperatorId::new("CASHIER-001").unwrap(),
            movement_type,
            direction,
            amount: money(minor),
            tendered: None,
            change: None,
            source_ref: None,
            reason_code: None,
            approval_id: None,
            now: ts(),
        }
    }

    #[test]
    fn an_opening_float_moves_cash_in() {
        let movement =
            CashMovement::record(new(MovementType::OpeningFloat, Direction::In, 100_000)).unwrap();
        assert_eq!(movement.signed_minor(), 100_000);
    }

    #[test]
    fn a_type_cannot_move_against_its_direction() {
        let error =
            CashMovement::record(new(MovementType::CashSale, Direction::Out, 100)).unwrap_err();
        assert_eq!(error.code(), "CASH_INVALID_MOVEMENT");
        assert!(CashMovement::record(new(MovementType::SafeDrop, Direction::In, 100)).is_err());
        // Every type moves exactly one way, so each one is checkable.
        for movement_type in [
            MovementType::OpeningFloat,
            MovementType::CashIn,
            MovementType::CashOut,
            MovementType::SafeDrop,
        ] {
            let wrong = match movement_type.direction() {
                Direction::In => Direction::Out,
                Direction::Out => Direction::In,
            };
            let mut candidate = new(movement_type, wrong, 100);
            candidate.reason_code = Some("REASON".to_owned());
            assert!(
                CashMovement::record(candidate).is_err(),
                "{} must not move {}",
                movement_type.as_str(),
                wrong.as_str()
            );
        }
    }

    #[test]
    fn amounts_are_always_positive() {
        for minor in [0, -1] {
            assert_eq!(
                CashMovement::record(new(MovementType::CashIn, Direction::In, minor))
                    .unwrap_err()
                    .code(),
                "CASH_INVALID_AMOUNT"
            );
        }
    }

    #[test]
    fn an_operator_movement_must_say_why() {
        let error =
            CashMovement::record(new(MovementType::CashIn, Direction::In, 100)).unwrap_err();
        assert_eq!(error.code(), "CASH_REASON_REQUIRED");
        // Whitespace is not a reason.
        let mut blank = new(MovementType::CashOut, Direction::Out, 100);
        blank.reason_code = Some("   ".to_owned());
        assert_eq!(
            CashMovement::record(blank).unwrap_err().code(),
            "CASH_REASON_REQUIRED"
        );
        let mut stated = new(MovementType::CashOut, Direction::Out, 100);
        stated.reason_code = Some("PETTY_CASH".to_owned());
        assert!(CashMovement::record(stated).is_ok());
    }

    #[test]
    fn a_runtime_movement_needs_no_reason() {
        // A sale and a float are caused by the runtime, not by an operator
        // deciding something, so there is nothing to explain.
        assert!(CashMovement::record(new(MovementType::OpeningFloat, Direction::In, 1)).is_ok());
    }

    #[test]
    fn a_cash_sale_records_what_stayed_in_the_drawer() {
        // 200.00 handed over for a 116.00 sale: 116.00 stays, 84.00 goes back.
        let mut sale = new(MovementType::CashSale, Direction::In, 11_600);
        sale.tendered = Some(money(20_000));
        sale.change = Some(money(8_400));
        sale.source_ref = Some("payment-1".to_owned());
        let movement = CashMovement::record(sale).unwrap();
        assert_eq!(movement.amount, money(11_600));
        assert_eq!(movement.tendered, Some(money(20_000)));
        assert_eq!(movement.change, Some(money(8_400)));
        assert_eq!(movement.signed_minor(), 11_600);
    }

    #[test]
    fn a_cash_sale_that_does_not_balance_is_refused() {
        let mut wrong = new(MovementType::CashSale, Direction::In, 11_600);
        wrong.tendered = Some(money(20_000));
        wrong.change = Some(money(1_000));
        let error = CashMovement::record(wrong).unwrap_err();
        assert_eq!(error.code(), "CASH_INVALID_MOVEMENT");
        assert!(matches!(error, CashError::UnbalancedCashSale { .. }));
    }

    #[test]
    fn a_cash_sale_must_carry_its_audit_and_nothing_else_may() {
        // Missing tendered/change.
        assert_eq!(
            CashMovement::record(new(MovementType::CashSale, Direction::In, 100))
                .unwrap_err()
                .code(),
            "CASH_INVALID_RECORD"
        );
        // A float with tender fields is a confused record.
        let mut float = new(MovementType::OpeningFloat, Direction::In, 100);
        float.tendered = Some(money(100));
        float.change = Some(money(0));
        assert_eq!(
            CashMovement::record(float).unwrap_err().code(),
            "CASH_INVALID_RECORD"
        );
    }
}
