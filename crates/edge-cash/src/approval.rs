//! Approval records.
//!
//! Deliberately not an authorisation system (ADR 023). The domain never asks
//! what role someone has; it is handed an identity the application layer has
//! already authorised, and its job is to remember who asked and who agreed.
//!
//! When real RBAC arrives it will decide *whether* to approve. This record
//! will keep saying who did.

use edge_domain::prelude::{OperatorId, Timestamp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CashError;

/// What is being approved. One variant today; the shape is what matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApprovalAction {
    SafeDrop,
}

impl ApprovalAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            ApprovalAction::SafeDrop => "SAFE_DROP",
        }
    }

    pub fn parse(value: &str) -> Result<Self, CashError> {
        match value {
            "SAFE_DROP" => Ok(ApprovalAction::SafeDrop),
            other => Err(CashError::UnknownEnumValue {
                kind: "ApprovalAction",
                value: other.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected,
}

impl ApprovalStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            ApprovalStatus::Pending => "PENDING",
            ApprovalStatus::Approved => "APPROVED",
            ApprovalStatus::Rejected => "REJECTED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, CashError> {
        match value {
            "PENDING" => Ok(ApprovalStatus::Pending),
            "APPROVED" => Ok(ApprovalStatus::Approved),
            "REJECTED" => Ok(ApprovalStatus::Rejected),
            other => Err(CashError::UnknownEnumValue {
                kind: "ApprovalStatus",
                value: other.to_owned(),
            }),
        }
    }

    pub const fn is_decided(self) -> bool {
        matches!(self, ApprovalStatus::Approved | ApprovalStatus::Rejected)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub approval_id: Uuid,
    pub action_type: ApprovalAction,
    /// What is being approved, by its own id.
    pub aggregate_ref: String,
    pub requested_by: OperatorId,
    pub requested_at: Timestamp,
    pub approved_by: Option<OperatorId>,
    pub decided_at: Option<Timestamp>,
    pub status: ApprovalStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewApproval {
    pub approval_id: Uuid,
    pub action_type: ApprovalAction,
    pub aggregate_ref: String,
    pub requested_by: OperatorId,
    pub reason: Option<String>,
    pub now: Timestamp,
}

impl Approval {
    pub fn request(new: NewApproval) -> Self {
        Self {
            approval_id: new.approval_id,
            action_type: new.action_type,
            aggregate_ref: new.aggregate_ref,
            requested_by: new.requested_by,
            requested_at: new.now,
            approved_by: None,
            decided_at: None,
            status: ApprovalStatus::Pending,
            reason: new.reason,
        }
    }

    pub fn rehydrate(approval: Self) -> Self {
        approval
    }

    /// Record a decision.
    ///
    /// The one rule enforced here is that the approver is not the requester:
    /// that is a property of the record, not of a role, so it belongs in the
    /// domain (ADR 023).
    pub fn decide(
        &mut self,
        approved_by: OperatorId,
        approve: bool,
        now: Timestamp,
    ) -> Result<(), CashError> {
        if self.status.is_decided() {
            // Deciding the same way twice is a replay, not an error.
            if self.approved_by.as_ref() == Some(&approved_by)
                && self.status
                    == if approve {
                        ApprovalStatus::Approved
                    } else {
                        ApprovalStatus::Rejected
                    }
            {
                return Ok(());
            }
            return Err(CashError::InvalidRecord {
                field: "approval_status",
                reason: format!("already {}", self.status.as_str()),
            });
        }
        if approved_by == self.requested_by {
            return Err(CashError::SelfApproval);
        }
        self.approved_by = Some(approved_by);
        self.decided_at = Some(now);
        self.status = if approve {
            ApprovalStatus::Approved
        } else {
            ApprovalStatus::Rejected
        };
        Ok(())
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> Timestamp {
        Timestamp::parse_rfc3339("2026-10-08T15:00:00Z").unwrap()
    }

    fn approval() -> Approval {
        Approval::request(NewApproval {
            approval_id: Uuid::from_u128(1),
            action_type: ApprovalAction::SafeDrop,
            aggregate_ref: "drop-1".to_owned(),
            requested_by: OperatorId::new("CASHIER-001").unwrap(),
            reason: Some("DRAWER_LIMIT".to_owned()),
            now: ts(),
        })
    }

    #[test]
    fn an_approval_starts_pending_and_undecided() {
        let approval = approval();
        assert_eq!(approval.status, ApprovalStatus::Pending);
        assert!(approval.approved_by.is_none());
        assert!(approval.decided_at.is_none());
    }

    #[test]
    fn the_requester_cannot_approve_their_own_request() {
        let mut approval = approval();
        let error = approval
            .decide(OperatorId::new("CASHIER-001").unwrap(), true, ts())
            .unwrap_err();
        assert_eq!(error.code(), "SAFE_DROP_SELF_APPROVAL");
        assert_eq!(approval.status, ApprovalStatus::Pending);
    }

    #[test]
    fn another_identity_can_approve_and_is_recorded() {
        let mut approval = approval();
        approval
            .decide(OperatorId::new("SUPERVISOR-01").unwrap(), true, ts())
            .unwrap();
        assert_eq!(approval.status, ApprovalStatus::Approved);
        assert_eq!(
            approval.approved_by.as_ref().map(ToString::to_string),
            Some("SUPERVISOR-01".to_owned())
        );
        assert_eq!(approval.decided_at, Some(ts()));
    }

    #[test]
    fn deciding_the_same_way_twice_is_a_replay_and_changing_it_is_refused() {
        let mut approval = approval();
        let supervisor = OperatorId::new("SUPERVISOR-01").unwrap();
        approval.decide(supervisor.clone(), true, ts()).unwrap();
        // Same approver, same decision: idempotent.
        approval.decide(supervisor, true, ts()).unwrap();
        assert_eq!(approval.status, ApprovalStatus::Approved);
        // A different decision after the fact is not an approval, it is a
        // rewrite of history.
        assert!(approval
            .decide(OperatorId::new("SUPERVISOR-02").unwrap(), false, ts())
            .is_err());
        assert_eq!(approval.status, ApprovalStatus::Approved);
    }

    #[test]
    fn a_request_can_be_rejected() {
        let mut approval = approval();
        approval
            .decide(OperatorId::new("SUPERVISOR-01").unwrap(), false, ts())
            .unwrap();
        assert_eq!(approval.status, ApprovalStatus::Rejected);
    }
}
