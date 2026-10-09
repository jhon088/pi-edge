//! Safe drops (ADR 023).
//!
//! Moving money out of a drawer into a safe is the one routine cash operation
//! a single person should not complete alone. A requested drop changes
//! nothing: the money is still in the drawer until someone with authority says
//! it left, and only then does a `SAFE_DROP` movement reduce expected cash.

use edge_domain::prelude::{Money, OperatorId, SessionId, Timestamp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CashError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SafeDropStatus {
    /// Requested, not yet decided. **Does not reduce expected cash.**
    PendingApproval,
    /// Approved and applied to the ledger.
    Approved,
    /// Declined. The money never left.
    Rejected,
}

impl SafeDropStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            SafeDropStatus::PendingApproval => "PENDING_APPROVAL",
            SafeDropStatus::Approved => "APPROVED",
            SafeDropStatus::Rejected => "REJECTED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, CashError> {
        match value {
            "PENDING_APPROVAL" => Ok(SafeDropStatus::PendingApproval),
            "APPROVED" => Ok(SafeDropStatus::Approved),
            "REJECTED" => Ok(SafeDropStatus::Rejected),
            other => Err(CashError::UnknownEnumValue {
                kind: "SafeDropStatus",
                value: other.to_owned(),
            }),
        }
    }

    /// Whether the drop is still waiting on a decision. A pending drop blocks
    /// a session close and a business-day close: the money's location is
    /// unknown.
    pub const fn is_pending(self) -> bool {
        matches!(self, SafeDropStatus::PendingApproval)
    }

    /// Whether the money has actually left the drawer.
    pub const fn is_applied(self) -> bool {
        matches!(self, SafeDropStatus::Approved)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafeDrop {
    pub safe_drop_id: Uuid,
    pub session_id: SessionId,
    pub approval_id: Uuid,
    pub amount: Money,
    pub reason_code: String,
    pub status: SafeDropStatus,
    pub requested_by: OperatorId,
    pub requested_at: Timestamp,
    pub approved_by: Option<OperatorId>,
    pub approved_at: Option<Timestamp>,
}

#[derive(Debug, Clone)]
pub struct NewSafeDrop {
    pub safe_drop_id: Uuid,
    pub session_id: SessionId,
    pub approval_id: Uuid,
    pub amount: Money,
    pub reason_code: String,
    pub requested_by: OperatorId,
    pub now: Timestamp,
}

impl SafeDrop {
    pub fn request(new: NewSafeDrop) -> Result<Self, CashError> {
        if !new.amount.is_positive() {
            return Err(CashError::NonPositiveAmount {
                minor: new.amount.minor(),
            });
        }
        let reason_code = new.reason_code.trim().to_owned();
        if reason_code.is_empty() {
            return Err(CashError::MissingReason {
                field: "reason_code",
            });
        }
        Ok(Self {
            safe_drop_id: new.safe_drop_id,
            session_id: new.session_id,
            approval_id: new.approval_id,
            amount: new.amount,
            reason_code,
            status: SafeDropStatus::PendingApproval,
            requested_by: new.requested_by,
            requested_at: new.now,
            approved_by: None,
            approved_at: None,
        })
    }

    pub fn rehydrate(drop: Self) -> Self {
        drop
    }

    /// Mark the drop approved. Re-approving by the same identity is a replay.
    ///
    /// The self-approval rule lives on the [`crate::Approval`] record, which
    /// this mirrors: both are checked, because the drop is what the ledger
    /// reads and the approval is what the audit reads.
    pub fn approve(&mut self, approved_by: OperatorId, now: Timestamp) -> Result<(), CashError> {
        match self.status {
            SafeDropStatus::PendingApproval => {}
            SafeDropStatus::Approved => {
                if self.approved_by.as_ref() == Some(&approved_by) {
                    return Ok(());
                }
                return Err(CashError::InvalidSafeDropTransition {
                    safe_drop_id: self.safe_drop_id.to_string(),
                    from: SafeDropStatus::Approved.as_str(),
                    attempted: "APPROVED",
                });
            }
            other => {
                return Err(CashError::InvalidSafeDropTransition {
                    safe_drop_id: self.safe_drop_id.to_string(),
                    from: other.as_str(),
                    attempted: "APPROVED",
                })
            }
        }
        if approved_by == self.requested_by {
            return Err(CashError::SelfApproval);
        }
        self.approved_by = Some(approved_by);
        self.approved_at = Some(now);
        self.status = SafeDropStatus::Approved;
        Ok(())
    }

    pub fn reject(&mut self, decided_by: OperatorId, now: Timestamp) -> Result<(), CashError> {
        match self.status {
            SafeDropStatus::PendingApproval => {}
            SafeDropStatus::Rejected => return Ok(()),
            other => {
                return Err(CashError::InvalidSafeDropTransition {
                    safe_drop_id: self.safe_drop_id.to_string(),
                    from: other.as_str(),
                    attempted: "REJECTED",
                })
            }
        }
        if decided_by == self.requested_by {
            return Err(CashError::SelfApproval);
        }
        self.approved_by = Some(decided_by);
        self.approved_at = Some(now);
        self.status = SafeDropStatus::Rejected;
        Ok(())
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use edge_domain::prelude::Currency;

    fn ts() -> Timestamp {
        Timestamp::parse_rfc3339("2026-10-08T15:00:00Z").unwrap()
    }

    fn drop_of(minor: i64, reason: &str) -> Result<SafeDrop, CashError> {
        SafeDrop::request(NewSafeDrop {
            safe_drop_id: Uuid::from_u128(1),
            session_id: SessionId::from_uuid(Uuid::from_u128(2)),
            approval_id: Uuid::from_u128(3),
            amount: Money::from_minor(minor, Currency::Mxn),
            reason_code: reason.to_owned(),
            requested_by: OperatorId::new("CASHIER-001").unwrap(),
            now: ts(),
        })
    }

    #[test]
    fn a_requested_drop_is_pending_and_has_not_moved_money() {
        let drop = drop_of(80_000, "DRAWER_LIMIT").unwrap();
        assert_eq!(drop.status, SafeDropStatus::PendingApproval);
        assert!(drop.status.is_pending());
        assert!(!drop.status.is_applied());
        assert!(drop.approved_by.is_none());
    }

    #[test]
    fn a_drop_needs_a_positive_amount_and_a_reason() {
        assert_eq!(
            drop_of(0, "DRAWER_LIMIT").unwrap_err().code(),
            "CASH_INVALID_AMOUNT"
        );
        assert_eq!(
            drop_of(100, "  ").unwrap_err().code(),
            "CASH_REASON_REQUIRED"
        );
    }

    #[test]
    fn the_requester_cannot_approve_their_own_drop() {
        let mut drop = drop_of(80_000, "DRAWER_LIMIT").unwrap();
        assert_eq!(
            drop.approve(OperatorId::new("CASHIER-001").unwrap(), ts())
                .unwrap_err()
                .code(),
            "SAFE_DROP_SELF_APPROVAL"
        );
        assert!(drop.status.is_pending(), "still pending after a refusal");
    }

    #[test]
    fn a_supervisor_approves_once_and_a_replay_is_harmless() {
        let mut drop = drop_of(80_000, "DRAWER_LIMIT").unwrap();
        let supervisor = OperatorId::new("SUPERVISOR-01").unwrap();
        drop.approve(supervisor.clone(), ts()).unwrap();
        assert!(drop.status.is_applied());
        assert_eq!(drop.approved_at, Some(ts()));
        // The same approver again changes nothing.
        drop.approve(supervisor, ts()).unwrap();
        assert!(drop.status.is_applied());
        // A different approver cannot re-approve an applied drop.
        assert!(drop
            .approve(OperatorId::new("SUPERVISOR-02").unwrap(), ts())
            .is_err());
    }

    #[test]
    fn a_rejected_drop_cannot_then_be_approved() {
        let mut drop = drop_of(80_000, "DRAWER_LIMIT").unwrap();
        drop.reject(OperatorId::new("SUPERVISOR-01").unwrap(), ts())
            .unwrap();
        assert_eq!(drop.status, SafeDropStatus::Rejected);
        assert_eq!(
            drop.approve(OperatorId::new("SUPERVISOR-01").unwrap(), ts())
                .unwrap_err()
                .code(),
            "SAFE_DROP_INVALID_STATE"
        );
    }
}
