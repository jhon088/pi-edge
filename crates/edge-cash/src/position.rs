//! Expected cash, derived from the ledger (ADR 021).
//!
//! There is no stored `expected_cash` to drift: the movements are the source of
//! truth, and this is the arithmetic over them. The one place the figure is
//! written down is a close declaration, which materialises what was used at
//! that instant so a historical variance is never recomputed under future
//! rules.

use edge_domain::prelude::{Currency, Money};
use serde::{Deserialize, Serialize};

use crate::error::CashError;
use crate::movement::{CashMovement, MovementType};

/// What a drawer should contain, and the figures that explain it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CashPosition {
    pub currency: Currency,
    pub opening_float: Money,
    /// Net cash from committed cash sales: what stayed in the drawer.
    pub cash_sales: Money,
    pub cash_in: Money,
    pub cash_out: Money,
    /// Approved drops only. A pending drop has not moved anything.
    pub safe_drops: Money,
    /// Cash handed back for commercial refunds.
    pub cash_refunds: Money,
    /// `Σ(IN) − Σ(OUT)`.
    pub expected_cash: Money,
    pub movement_count: i64,
    /// Audit only: what customers handed over in cash.
    pub tendered_total: Money,
    /// Audit only: what went back as change.
    pub change_total: Money,
}

impl CashPosition {
    pub fn empty(currency: Currency) -> Self {
        let zero = Money::zero(currency);
        Self {
            currency,
            opening_float: zero,
            cash_sales: zero,
            cash_in: zero,
            cash_out: zero,
            safe_drops: zero,
            cash_refunds: zero,
            expected_cash: zero,
            movement_count: 0,
            tendered_total: zero,
            change_total: zero,
        }
    }
}

/// Fold a session's movements into its position.
///
/// Order does not matter: addition is commutative and no movement depends on
/// another. A replayed ledger therefore always yields the same figure.
pub fn position(movements: &[CashMovement], currency: Currency) -> Result<CashPosition, CashError> {
    let mut position = CashPosition::empty(currency);
    for movement in movements {
        let amount = movement.amount;
        match (movement.movement_type, movement.direction) {
            (MovementType::OpeningFloat, _) => {
                position.opening_float = position.opening_float.checked_add(amount)?;
            }
            (MovementType::CashSale, _) => {
                position.cash_sales = position.cash_sales.checked_add(amount)?;
                if let Some(tendered) = movement.tendered {
                    position.tendered_total = position.tendered_total.checked_add(tendered)?;
                }
                if let Some(change) = movement.change {
                    position.change_total = position.change_total.checked_add(change)?;
                }
            }
            (MovementType::CashIn, _) => {
                position.cash_in = position.cash_in.checked_add(amount)?;
            }
            (MovementType::CashOut, _) => {
                position.cash_out = position.cash_out.checked_add(amount)?;
            }
            (MovementType::SafeDrop, _) => {
                position.safe_drops = position.safe_drops.checked_add(amount)?;
            }
            (MovementType::CashRefund, _) => {
                position.cash_refunds = position.cash_refunds.checked_add(amount)?;
            }
        }
        position.movement_count += 1;
    }

    let inflow = position
        .opening_float
        .checked_add(position.cash_sales)?
        .checked_add(position.cash_in)?;
    let outflow = position
        .cash_out
        .checked_add(position.safe_drops)?
        .checked_add(position.cash_refunds)?;
    // Expected cash can legitimately be negative if a drawer was over-emptied,
    // so it is not clamped: hiding that would hide a real problem.
    position.expected_cash = inflow.checked_sub(outflow)?;
    Ok(position)
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use edge_domain::prelude::{BusinessDayId, OperatorId, RegisterId, SessionId, Timestamp};
    use uuid::Uuid;

    use crate::movement::{Direction, NewCashMovement};

    const MXN: Currency = Currency::Mxn;

    fn money(minor: i64) -> Money {
        Money::from_minor(minor, MXN)
    }

    fn ts() -> Timestamp {
        Timestamp::parse_rfc3339("2026-10-08T15:00:00Z").unwrap()
    }

    fn movement(
        seed: u128,
        movement_type: MovementType,
        direction: Direction,
        minor: i64,
    ) -> CashMovement {
        let needs_reason = movement_type.requires_reason();
        let sale = movement_type == MovementType::CashSale;
        CashMovement::record(NewCashMovement {
            movement_id: Uuid::from_u128(seed),
            business_day_id: BusinessDayId::from_uuid(Uuid::from_u128(900)),
            session_id: SessionId::from_uuid(Uuid::from_u128(901)),
            register_id: RegisterId::new("REG-001").unwrap(),
            operator_id: OperatorId::new("CASHIER-001").unwrap(),
            movement_type,
            direction,
            amount: money(minor),
            tendered: sale.then(|| money(minor)),
            change: sale.then(|| money(0)),
            source_ref: sale.then(|| format!("payment-{seed}")),
            reason_code: needs_reason.then(|| "REASON".to_owned()),
            approval_id: None,
            now: ts(),
        })
        .unwrap()
    }

    #[test]
    fn an_empty_ledger_expects_nothing() {
        let position = position(&[], MXN).unwrap();
        assert_eq!(position.expected_cash, money(0));
        assert_eq!(position.movement_count, 0);
    }

    #[test]
    fn an_opening_float_is_the_whole_expectation_at_first() {
        let ledger = vec![movement(
            1,
            MovementType::OpeningFloat,
            Direction::In,
            100_000,
        )];
        let position = position(&ledger, MXN).unwrap();
        assert_eq!(position.opening_float, money(100_000));
        assert_eq!(position.expected_cash, money(100_000));
    }

    #[test]
    fn inflows_add_and_outflows_subtract() {
        let ledger = vec![
            movement(1, MovementType::OpeningFloat, Direction::In, 100_000),
            movement(2, MovementType::CashSale, Direction::In, 11_600),
            movement(3, MovementType::CashIn, Direction::In, 50_000),
            movement(4, MovementType::CashOut, Direction::Out, 10_000),
            movement(5, MovementType::SafeDrop, Direction::Out, 80_000),
        ];
        let position = position(&ledger, MXN).unwrap();
        // 1000 + 116 + 500 - 100 - 800 = 716
        assert_eq!(position.expected_cash, money(71_600));
        assert_eq!(position.cash_sales, money(11_600));
        assert_eq!(position.cash_in, money(50_000));
        assert_eq!(position.cash_out, money(10_000));
        assert_eq!(position.safe_drops, money(80_000));
        assert_eq!(position.movement_count, 5);
    }

    #[test]
    fn the_order_of_movements_does_not_change_the_position() {
        let ledger = vec![
            movement(1, MovementType::OpeningFloat, Direction::In, 100_000),
            movement(2, MovementType::CashSale, Direction::In, 11_600),
            movement(3, MovementType::CashOut, Direction::Out, 10_000),
        ];
        let forward = position(&ledger, MXN).unwrap();
        let reversed: Vec<CashMovement> = ledger.iter().rev().cloned().collect();
        assert_eq!(position(&reversed, MXN).unwrap(), forward);
    }

    #[test]
    fn a_cash_sale_with_change_only_counts_what_stayed() {
        // 200.00 tendered for a 116.00 sale: the drawer gains 116.00, and the
        // audit of the 200/84 is kept without distorting the expectation.
        let sale = CashMovement::record(NewCashMovement {
            movement_id: Uuid::from_u128(7),
            business_day_id: BusinessDayId::from_uuid(Uuid::from_u128(900)),
            session_id: SessionId::from_uuid(Uuid::from_u128(901)),
            register_id: RegisterId::new("REG-001").unwrap(),
            operator_id: OperatorId::new("CASHIER-001").unwrap(),
            movement_type: MovementType::CashSale,
            direction: Direction::In,
            amount: money(11_600),
            tendered: Some(money(20_000)),
            change: Some(money(8_400)),
            source_ref: Some("payment-1".to_owned()),
            reason_code: None,
            approval_id: None,
            now: ts(),
        })
        .unwrap();
        let ledger = vec![
            movement(1, MovementType::OpeningFloat, Direction::In, 100_000),
            sale,
        ];
        let position = position(&ledger, MXN).unwrap();
        assert_eq!(position.expected_cash, money(111_600));
        assert_eq!(position.tendered_total, money(20_000));
        assert_eq!(position.change_total, money(8_400));
    }

    #[test]
    fn an_over_emptied_drawer_reports_a_negative_expectation() {
        // Clamping this to zero would hide a real operational problem.
        let ledger = vec![
            movement(1, MovementType::OpeningFloat, Direction::In, 10_000),
            movement(2, MovementType::CashOut, Direction::Out, 15_000),
        ];
        assert_eq!(position(&ledger, MXN).unwrap().expected_cash, money(-5_000));
    }
}
