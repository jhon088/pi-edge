//! `PaymentAttempt`: one concrete interaction with a payment provider.
//!
//! Append-only. A new attempt never rewrites an older one, so the history of
//! what was asked and what came back survives every retry and every restart
//! (ADR 016). The state machine is ADR 017.

use edge_domain::prelude::{Money, PaymentId, Timestamp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::PaymentError;
use crate::tender::ProviderKind;

const AGGREGATE: &str = "PaymentAttempt";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AttemptStatus {
    /// Recorded, not yet sent.
    Created,
    /// Sent to the provider, no result yet. **Unresolved**: a process that
    /// died here cannot know the outcome, so this state means "we asked and we
    /// do not know" and is reconcilable. Nothing ever turns it into `FAILED`
    /// on its own (ADR 018).
    Processing,
    /// The provider approved. The money is not applied yet.
    Authorized,
    /// The provider was asked and the result is genuinely unknown.
    Unknown,
    /// Terminal: the provider refused, or a query proved nothing was charged.
    Failed,
    /// A reversal was requested and is not confirmed. **Unresolved.**
    ReversalPending,
    /// Terminal: the provider confirmed the reversal.
    Reversed,
}

impl AttemptStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            AttemptStatus::Created => "CREATED",
            AttemptStatus::Processing => "PROCESSING",
            AttemptStatus::Authorized => "AUTHORIZED",
            AttemptStatus::Unknown => "UNKNOWN",
            AttemptStatus::Failed => "FAILED",
            AttemptStatus::ReversalPending => "REVERSAL_PENDING",
            AttemptStatus::Reversed => "REVERSED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, PaymentError> {
        match value {
            "CREATED" => Ok(AttemptStatus::Created),
            "PROCESSING" => Ok(AttemptStatus::Processing),
            "AUTHORIZED" => Ok(AttemptStatus::Authorized),
            "UNKNOWN" => Ok(AttemptStatus::Unknown),
            "FAILED" => Ok(AttemptStatus::Failed),
            "REVERSAL_PENDING" => Ok(AttemptStatus::ReversalPending),
            "REVERSED" => Ok(AttemptStatus::Reversed),
            other => Err(PaymentError::UnknownEnumValue {
                kind: "AttemptStatus",
                value: other.to_owned(),
            }),
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, AttemptStatus::Failed | AttemptStatus::Reversed)
    }

    /// Whether the financial outcome is still open. While any attempt of a
    /// sale is unresolved, the sale cannot finalize and no new attempt may be
    /// started that could duplicate the same charge (ADR 018).
    pub const fn is_unresolved(self) -> bool {
        matches!(
            self,
            AttemptStatus::Created
                | AttemptStatus::Processing
                | AttemptStatus::Unknown
                | AttemptStatus::ReversalPending
        )
    }

    /// Whether an explicit provider query is the way out of this state.
    pub const fn requires_reconciliation(self) -> bool {
        matches!(
            self,
            AttemptStatus::Processing | AttemptStatus::Unknown | AttemptStatus::ReversalPending
        )
    }

    pub const fn blocks_basket(self) -> bool {
        !self.is_terminal()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentAttempt {
    pub attempt_id: Uuid,
    pub payment_id: PaymentId,
    /// 1-based, never reused within a payment.
    pub attempt_number: i64,
    pub status: AttemptStatus,
    pub requested_amount: Money,
    pub authorized_amount: Option<Money>,
    pub provider_kind: ProviderKind,
    /// Minted by the core and committed **before** the provider is called. It
    /// is what a provider is asked about during reconciliation, and what makes
    /// a provider-side retry idempotent (ADR 020).
    pub correlation_id: Uuid,
    /// The provider's own identifier, recorded when it answers.
    pub provider_reference: Option<String>,
    /// Stable provider reason for a refusal. Never a raw provider payload.
    pub failure_code: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct NewPaymentAttempt {
    pub attempt_id: Uuid,
    pub payment_id: PaymentId,
    pub attempt_number: i64,
    pub requested_amount: Money,
    pub provider_kind: ProviderKind,
    pub correlation_id: Uuid,
    pub now: Timestamp,
}

impl PaymentAttempt {
    /// Record an attempt in `CREATED`.
    pub fn open(new: NewPaymentAttempt) -> Result<Self, PaymentError> {
        if !new.requested_amount.is_positive() {
            return Err(PaymentError::NonPositiveAmount {
                minor: new.requested_amount.minor(),
            });
        }
        if new.attempt_number < 1 {
            return Err(PaymentError::InvalidRecord {
                field: "attempt_number",
                reason: "must be 1 or greater".to_owned(),
            });
        }
        Ok(Self {
            attempt_id: new.attempt_id,
            payment_id: new.payment_id,
            attempt_number: new.attempt_number,
            status: AttemptStatus::Created,
            requested_amount: new.requested_amount,
            authorized_amount: None,
            provider_kind: new.provider_kind,
            correlation_id: new.correlation_id,
            provider_reference: None,
            failure_code: None,
            created_at: new.now,
            updated_at: new.now,
        })
    }

    /// About to be sent. Committed before the provider call (ADR 020).
    pub fn mark_processing(&mut self, now: Timestamp) -> Result<(), PaymentError> {
        match self.status {
            AttemptStatus::Created => {
                self.status = AttemptStatus::Processing;
                self.updated_at = now;
                Ok(())
            }
            AttemptStatus::Processing => Ok(()),
            other => Err(self.invalid(other, "PROCESSING")),
        }
    }

    /// The provider approved. Re-applying the same result is a replay.
    pub fn mark_authorized(
        &mut self,
        authorized: Money,
        provider_reference: Option<String>,
        now: Timestamp,
    ) -> Result<(), PaymentError> {
        match self.status {
            AttemptStatus::Processing | AttemptStatus::Unknown => {}
            AttemptStatus::Authorized => return Ok(()),
            other => return Err(self.invalid(other, "AUTHORIZED")),
        }
        self.status = AttemptStatus::Authorized;
        self.authorized_amount = Some(authorized);
        if provider_reference.is_some() {
            self.provider_reference = provider_reference;
        }
        self.updated_at = now;
        Ok(())
    }

    /// The provider refused, or a query proved nothing was charged.
    ///
    /// Note what is *not* here: there is no path from `AUTHORIZED` to
    /// `FAILED`. Undoing an authorization is a reversal, not a failure.
    pub fn mark_failed(
        &mut self,
        failure_code: Option<String>,
        now: Timestamp,
    ) -> Result<(), PaymentError> {
        match self.status {
            AttemptStatus::Processing | AttemptStatus::Unknown => {}
            AttemptStatus::Failed => return Ok(()),
            other => return Err(self.invalid(other, "FAILED")),
        }
        self.status = AttemptStatus::Failed;
        self.failure_code = failure_code;
        self.updated_at = now;
        Ok(())
    }

    /// We asked and we do not know.
    pub fn mark_unknown(&mut self, now: Timestamp) -> Result<(), PaymentError> {
        match self.status {
            AttemptStatus::Processing => {
                self.status = AttemptStatus::Unknown;
                self.updated_at = now;
                Ok(())
            }
            AttemptStatus::Unknown => Ok(()),
            other => Err(self.invalid(other, "UNKNOWN")),
        }
    }

    /// A reversal has been requested and is not confirmed.
    pub fn mark_reversal_pending(&mut self, now: Timestamp) -> Result<(), PaymentError> {
        match self.status {
            AttemptStatus::Authorized => {
                self.status = AttemptStatus::ReversalPending;
                self.updated_at = now;
                Ok(())
            }
            AttemptStatus::ReversalPending => Ok(()),
            other => Err(PaymentError::ReversalNotAllowed {
                payment_id: self.payment_id.to_string(),
                status: other.as_str(),
            }),
        }
    }

    /// The provider confirmed the reversal.
    pub fn mark_reversed(&mut self, now: Timestamp) -> Result<(), PaymentError> {
        match self.status {
            AttemptStatus::ReversalPending => {
                self.status = AttemptStatus::Reversed;
                self.updated_at = now;
                Ok(())
            }
            AttemptStatus::Reversed => Ok(()),
            other => Err(PaymentError::ReversalNotAllowed {
                payment_id: self.payment_id.to_string(),
                status: other.as_str(),
            }),
        }
    }

    fn invalid(&self, from: AttemptStatus, attempted: &'static str) -> PaymentError {
        PaymentError::InvalidTransition {
            aggregate: AGGREGATE,
            from: from.as_str(),
            attempted,
        }
    }
}
