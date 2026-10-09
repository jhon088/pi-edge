//! The `Payment` aggregate: one tender's intention to cover part of a sale.
//!
//! The state machine is ADR 017. Three amounts are kept apart on purpose:
//! `requested` (what this tender was asked for, i.e. what the customer handed
//! over in cash), `authorized` (what a provider approved) and `committed`
//! (what is applied against the sale's debt). Change is the gap between
//! requested and committed, and only cash can produce it.

use edge_domain::prelude::{Money, PaymentId, Timestamp, TransactionId};
use serde::{Deserialize, Serialize};

use crate::error::PaymentError;
use crate::tender::{ProviderKind, TenderType};

const AGGREGATE: &str = "Payment";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PaymentStatus {
    /// Durably recorded, nothing attempted yet. Already blocks the basket: the
    /// record exists precisely because it is written before a provider is
    /// called (ADR 020).
    Created,
    /// An attempt is live or unresolved.
    InProgress,
    /// `committed_amount` is applied to the sale.
    Committed,
    /// Terminal: nothing was applied.
    Failed,
    /// Terminal: something was applied and then undone (ADR 019).
    Reversed,
}

impl PaymentStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            PaymentStatus::Created => "CREATED",
            PaymentStatus::InProgress => "IN_PROGRESS",
            PaymentStatus::Committed => "COMMITTED",
            PaymentStatus::Failed => "FAILED",
            PaymentStatus::Reversed => "REVERSED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, PaymentError> {
        match value {
            "CREATED" => Ok(PaymentStatus::Created),
            "IN_PROGRESS" => Ok(PaymentStatus::InProgress),
            "COMMITTED" => Ok(PaymentStatus::Committed),
            "FAILED" => Ok(PaymentStatus::Failed),
            "REVERSED" => Ok(PaymentStatus::Reversed),
            other => Err(PaymentError::UnknownEnumValue {
                kind: "PaymentStatus",
                value: other.to_owned(),
            }),
        }
    }

    /// Nothing further can happen to the payment.
    pub const fn is_terminal(self) -> bool {
        matches!(self, PaymentStatus::Failed | PaymentStatus::Reversed)
    }

    /// Whether the payment's money counts against the sale's debt.
    pub const fn is_applied(self) -> bool {
        matches!(self, PaymentStatus::Committed)
    }

    /// Whether the payment keeps the basket frozen (ADR 019). Everything that
    /// is not terminal does, `CREATED` included.
    pub const fn blocks_basket(self) -> bool {
        !self.is_terminal()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payment {
    pub payment_id: PaymentId,
    pub transaction_id: TransactionId,
    pub tender_type: TenderType,
    /// Set exactly when the tender is settled through a provider.
    pub provider_kind: Option<ProviderKind>,
    /// What this tender was asked to cover. For cash, what the customer
    /// handed over.
    pub requested_amount: Money,
    /// What a provider approved. `None` for cash and for unresolved payments.
    pub authorized_amount: Option<Money>,
    /// What is applied against the sale's debt. Zero until committed.
    pub committed_amount: Money,
    pub status: PaymentStatus,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct NewPayment {
    pub payment_id: PaymentId,
    pub transaction_id: TransactionId,
    pub tender_type: TenderType,
    pub provider_kind: Option<ProviderKind>,
    pub requested_amount: Money,
    pub now: Timestamp,
}

impl Payment {
    /// Open a payment. It exists durably before anything is attempted.
    pub fn open(new: NewPayment) -> Result<Self, PaymentError> {
        if !new.requested_amount.is_positive() {
            return Err(PaymentError::NonPositiveAmount {
                minor: new.requested_amount.minor(),
            });
        }
        if new.tender_type.uses_provider() != new.provider_kind.is_some() {
            return Err(PaymentError::InvalidRecord {
                field: "provider_kind",
                reason: format!(
                    "{} must {}name a provider",
                    new.tender_type.as_str(),
                    if new.tender_type.uses_provider() {
                        ""
                    } else {
                        "not "
                    }
                ),
            });
        }
        Ok(Self {
            payment_id: new.payment_id,
            transaction_id: new.transaction_id,
            tender_type: new.tender_type,
            provider_kind: new.provider_kind,
            requested_amount: new.requested_amount,
            authorized_amount: None,
            committed_amount: Money::zero(new.requested_amount.currency()),
            status: PaymentStatus::Created,
            created_at: new.now,
            updated_at: new.now,
        })
    }

    /// Rebuild from storage without re-running the opening rules.
    pub fn rehydrate(payment: Self) -> Self {
        payment
    }

    /// An attempt is now live with a provider.
    pub fn mark_in_progress(&mut self, now: Timestamp) -> Result<(), PaymentError> {
        match self.status {
            PaymentStatus::Created | PaymentStatus::InProgress => {
                self.status = PaymentStatus::InProgress;
                self.updated_at = now;
                Ok(())
            }
            other => Err(self.invalid(other, "IN_PROGRESS")),
        }
    }

    /// Record what a provider authorized. Does not apply the money.
    pub fn mark_authorized(
        &mut self,
        authorized: Money,
        now: Timestamp,
    ) -> Result<(), PaymentError> {
        match self.status {
            PaymentStatus::Created | PaymentStatus::InProgress => {
                self.authorized_amount = Some(authorized);
                self.status = PaymentStatus::InProgress;
                self.updated_at = now;
                Ok(())
            }
            other => Err(self.invalid(other, "AUTHORIZED")),
        }
    }

    /// Apply `committed` against the sale's debt.
    ///
    /// `committed` is decided by the settlement rules, not by the payment: for
    /// cash it is `min(requested, remaining)`, for a card it is what the
    /// provider authorized.
    pub fn commit(&mut self, committed: Money, now: Timestamp) -> Result<(), PaymentError> {
        match self.status {
            PaymentStatus::Created | PaymentStatus::InProgress => {}
            PaymentStatus::Committed => {
                // Committing the same amount twice is a replay, not an error.
                if self.committed_amount == committed {
                    return Ok(());
                }
                return Err(PaymentError::AlreadyResolved {
                    payment_id: self.payment_id.to_string(),
                    status: PaymentStatus::Committed.as_str(),
                });
            }
            other => return Err(self.invalid(other, "COMMITTED")),
        }
        if committed.is_negative() {
            return Err(PaymentError::NonPositiveAmount {
                minor: committed.minor(),
            });
        }
        if committed.minor() > self.requested_amount.minor() {
            return Err(PaymentError::InvalidRecord {
                field: "committed_amount",
                reason: "must not exceed the requested amount".to_owned(),
            });
        }
        self.committed_amount = committed;
        self.status = PaymentStatus::Committed;
        self.updated_at = now;
        Ok(())
    }

    /// The provider refused, or reconciliation proved nothing was charged.
    pub fn mark_failed(&mut self, now: Timestamp) -> Result<(), PaymentError> {
        match self.status {
            PaymentStatus::Created | PaymentStatus::InProgress => {
                self.status = PaymentStatus::Failed;
                self.committed_amount = Money::zero(self.requested_amount.currency());
                self.updated_at = now;
                Ok(())
            }
            PaymentStatus::Failed => Ok(()),
            other => Err(self.invalid(other, "FAILED")),
        }
    }

    /// The payment has been undone at the provider (ADR 019).
    pub fn mark_reversed(&mut self, now: Timestamp) -> Result<(), PaymentError> {
        match self.status {
            PaymentStatus::InProgress | PaymentStatus::Committed => {
                self.status = PaymentStatus::Reversed;
                self.committed_amount = Money::zero(self.requested_amount.currency());
                self.updated_at = now;
                Ok(())
            }
            PaymentStatus::Reversed => Ok(()),
            other => Err(PaymentError::ReversalNotAllowed {
                payment_id: self.payment_id.to_string(),
                status: other.as_str(),
            }),
        }
    }

    /// Change owed back for this payment: what was handed over minus what was
    /// applied. Only a tender that may overpay can produce it.
    pub fn change_due(&self) -> Result<Money, PaymentError> {
        if !self.status.is_applied() || !self.tender_type.may_overpay() {
            return Ok(Money::zero(self.requested_amount.currency()));
        }
        Ok(self
            .requested_amount
            .checked_sub(self.committed_amount)?
            .clamp_non_negative())
    }

    fn invalid(&self, from: PaymentStatus, attempted: &'static str) -> PaymentError {
        PaymentError::InvalidTransition {
            aggregate: AGGREGATE,
            from: from.as_str(),
            attempted,
        }
    }
}
