//! The blind close (ADR 022).
//!
//! A cashier declares what the drawer contains **before** learning what it
//! should contain. That is enforced by the contract, not by the interface:
//! [`SessionCloseRequirements`] has no field that could carry an amount, and
//! the expected figure appears only in the result of a declaration.

use edge_domain::prelude::{ClosureKind, Currency, Money, OperatorId, SessionId, Timestamp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CashError;

/// What a session still needs before it can close.
///
/// **This type must never gain a field that reveals expected cash** — not the
/// figure, not a movement list, not a running total. A serialization test
/// asserts that its rendered JSON contains neither the words nor the amount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCloseRequirements {
    pub session_id: SessionId,
    pub currency: Currency,
    /// How many movements the drawer has. A count, never an amount.
    pub movement_count: i64,
    /// Everything standing in the way, reported together so an operator does
    /// not discover them one refusal at a time.
    pub blockers: Vec<String>,
    pub requires_declaration: bool,
}

impl SessionCloseRequirements {
    pub fn can_declare(&self) -> bool {
        self.blockers.is_empty()
    }
}

/// What an operator counted, against what the ledger said at that instant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseDeclaration {
    pub declaration_id: Uuid,
    pub session_id: SessionId,
    pub closure_kind: ClosureKind,
    /// Absent for a forced close: no count was taken, and inventing one would
    /// manufacture a clean variance out of nothing.
    pub declared_cash: Option<Money>,
    /// The ledger figure used at this close. Materialised so a historical
    /// variance is never recomputed under future rules.
    pub expected_at_close: Money,
    /// `declared − expected`. Absent exactly when `declared_cash` is.
    pub variance: Option<Money>,
    pub declared_by: Option<OperatorId>,
    pub declared_at: Timestamp,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewCloseDeclaration {
    pub declaration_id: Uuid,
    pub session_id: SessionId,
    pub expected_at_close: Money,
    pub now: Timestamp,
}

impl CloseDeclaration {
    /// A counted close. `variance = declared − expected`; a shortage is
    /// negative, an overage positive, and neither is corrected automatically.
    pub fn declared(
        new: NewCloseDeclaration,
        declared_cash: Money,
        declared_by: OperatorId,
    ) -> Result<Self, CashError> {
        if declared_cash.is_negative() {
            return Err(CashError::NonPositiveAmount {
                minor: declared_cash.minor(),
            });
        }
        let variance = declared_cash.checked_sub(new.expected_at_close)?;
        Ok(Self {
            declaration_id: new.declaration_id,
            session_id: new.session_id,
            closure_kind: ClosureKind::Declared,
            declared_cash: Some(declared_cash),
            expected_at_close: new.expected_at_close,
            variance: Some(variance),
            declared_by: Some(declared_by),
            declared_at: new.now,
            reason: None,
        })
    }

    /// A forced close. No count, so no variance — the expected figure is still
    /// recorded, because it is a fact about the ledger rather than a claim
    /// about the drawer (ADR 024).
    pub fn forced(new: NewCloseDeclaration, reason: String) -> Result<Self, CashError> {
        let reason = reason.trim().to_owned();
        if reason.is_empty() {
            return Err(CashError::MissingReason { field: "reason" });
        }
        Ok(Self {
            declaration_id: new.declaration_id,
            session_id: new.session_id,
            closure_kind: ClosureKind::Forced,
            declared_cash: None,
            expected_at_close: new.expected_at_close,
            variance: None,
            declared_by: None,
            declared_at: new.now,
            reason: Some(reason),
        })
    }

    pub fn rehydrate(declaration: Self) -> Self {
        declaration
    }

    /// Whether the drawer was actually counted.
    pub fn is_declared(&self) -> bool {
        self.closure_kind == ClosureKind::Declared
    }

    /// A short label for reports, where "unknown" must not read as zero.
    pub fn variance_label(&self) -> &'static str {
        match self.variance {
            None => "UNDECLARED",
            Some(variance) if variance.minor() == 0 => "BALANCED",
            Some(variance) if variance.is_negative() => "SHORTAGE",
            Some(_) => "OVERAGE",
        }
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    const MXN: Currency = Currency::Mxn;

    fn money(minor: i64) -> Money {
        Money::from_minor(minor, MXN)
    }

    fn ts() -> Timestamp {
        Timestamp::parse_rfc3339("2026-10-08T20:00:00Z").unwrap()
    }

    fn new(expected_minor: i64) -> NewCloseDeclaration {
        NewCloseDeclaration {
            declaration_id: Uuid::from_u128(1),
            session_id: SessionId::from_uuid(Uuid::from_u128(2)),
            expected_at_close: money(expected_minor),
            now: ts(),
        }
    }

    fn cashier() -> OperatorId {
        OperatorId::new("CASHIER-001").unwrap()
    }

    #[test]
    fn an_exact_count_has_no_variance() {
        let declaration =
            CloseDeclaration::declared(new(105_000), money(105_000), cashier()).unwrap();
        assert_eq!(declaration.variance, Some(money(0)));
        assert_eq!(declaration.variance_label(), "BALANCED");
        assert!(declaration.is_declared());
    }

    #[test]
    fn a_shortage_is_negative_and_an_overage_positive() {
        let short = CloseDeclaration::declared(new(105_000), money(104_000), cashier()).unwrap();
        assert_eq!(short.variance, Some(money(-1_000)));
        assert_eq!(short.variance_label(), "SHORTAGE");

        let over = CloseDeclaration::declared(new(105_000), money(106_500), cashier()).unwrap();
        assert_eq!(over.variance, Some(money(1_500)));
        assert_eq!(over.variance_label(), "OVERAGE");
    }

    #[test]
    fn the_expected_figure_is_materialised_on_the_declaration() {
        let declaration =
            CloseDeclaration::declared(new(105_000), money(104_000), cashier()).unwrap();
        // Later movements or rule changes cannot move this number.
        assert_eq!(declaration.expected_at_close, money(105_000));
    }

    #[test]
    fn a_forced_close_invents_no_count_and_no_variance() {
        let declaration = CloseDeclaration::forced(new(105_000), "central EOD".to_owned()).unwrap();
        assert_eq!(declaration.closure_kind, ClosureKind::Forced);
        assert!(declaration.declared_cash.is_none(), "no count was taken");
        assert!(declaration.variance.is_none(), "unknown, not zero");
        assert_eq!(declaration.variance_label(), "UNDECLARED");
        assert_eq!(declaration.expected_at_close, money(105_000));
        assert!(!declaration.is_declared());
    }

    #[test]
    fn a_forced_close_must_state_a_reason() {
        assert_eq!(
            CloseDeclaration::forced(new(1), "   ".to_owned())
                .unwrap_err()
                .code(),
            "CASH_REASON_REQUIRED"
        );
    }

    #[test]
    fn a_negative_count_is_refused() {
        assert_eq!(
            CloseDeclaration::declared(new(1_000), money(-1), cashier())
                .unwrap_err()
                .code(),
            "CASH_INVALID_AMOUNT"
        );
    }

    #[test]
    fn the_pre_close_requirements_cannot_reveal_the_expected_figure() {
        // The structural half of the blind close: this is the DTO a cashier's
        // close path receives, and there is nothing in it to leak.
        let requirements = SessionCloseRequirements {
            session_id: SessionId::from_uuid(Uuid::from_u128(2)),
            currency: MXN,
            movement_count: 7,
            blockers: Vec::new(),
            requires_declaration: true,
        };
        let json = serde_json::to_string(&requirements).unwrap();
        for forbidden in ["expected", "105000", "1050.00", "amount", "total"] {
            assert!(
                !json.contains(forbidden),
                "`{forbidden}` must not appear in the pre-close payload: {json}"
            );
        }
        assert!(requirements.can_declare());
    }
}
