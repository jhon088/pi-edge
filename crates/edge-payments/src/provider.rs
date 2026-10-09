//! The payment provider port.
//!
//! Four capabilities, no more: start a payment, ask what happened to one, undo
//! one, and give money back for one. Everything crosses the boundary as domain
//! types — never a provider's JSON, never a raw terminal payload.
//!
//! `refund_payment` arrived in milestone 9, when commercial refunds gave it a
//! real consumer; until then it was deliberately absent (ADR 019, amended).
//! **It is not `reverse_payment` under another name**: a reversal un-applies a
//! charge that should not have stayed, inside the lifecycle of the original
//! payment, while a refund gives money back for a sale that already closed and
//! belongs to a refund document (ADR 034). The two are separate methods so a
//! provider implementation cannot quietly serve one with the other.

use edge_domain::prelude::{Money, Timestamp};
use uuid::Uuid;

use crate::error::PaymentError;
use crate::tender::{ProviderKind, TenderType};

/// What the core asks a provider to charge.
///
/// `correlation_id` is minted by the core and committed before this call, so
/// the provider can be asked about it later and a provider-side retry is
/// idempotent (ADR 020).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequest {
    pub correlation_id: Uuid,
    pub tender_type: TenderType,
    pub amount: Money,
    pub requested_at: Timestamp,
}

/// Ask what happened to an interaction we already started. Read-only: this
/// must never create a charge (ADR 018).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderQuery {
    pub correlation_id: Uuid,
    pub provider_reference: Option<String>,
    /// What the core believes was requested. A provider that knows better
    /// answers with its own figure in [`ProviderOutcome::Authorized`].
    pub amount: Money,
}

/// Undo an interaction that must not remain applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderReversal {
    pub correlation_id: Uuid,
    pub provider_reference: Option<String>,
    pub amount: Money,
}

/// What a provider says about one interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderOutcome {
    Authorized {
        authorized: Money,
        provider_reference: String,
    },
    Declined {
        /// Stable reason code. Never a free-form provider payload.
        failure_code: String,
    },
    /// The provider could not be reached, or answered nothing usable. The
    /// charge may or may not exist. This is a legitimate answer, not an error
    /// (ADR 018).
    Unknown,
}

/// Give money back for a charge that is settled.
///
/// `correlation_id` is the **refund's** own, minted by the core and committed
/// before this call; `original_provider_reference` is what the provider gave
/// when it took the money, so it can find the charge being refunded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefundRequest {
    pub correlation_id: Uuid,
    pub tender_type: TenderType,
    pub amount: Money,
    pub original_provider_reference: Option<String>,
    pub requested_at: Timestamp,
}

/// What a provider says about a refund.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefundOutcome {
    Refunded {
        refunded: Money,
        provider_reference: String,
    },
    Declined {
        /// Stable reason code. Never a free-form provider payload.
        failure_code: String,
    },
    /// The provider could not be reached, or answered nothing usable. The
    /// money may or may not have gone back, and the only way forward is to ask
    /// about this same `correlation_id` — never to send a second refund
    /// (ADR 018, applied to refunds).
    Unknown,
}

/// What a provider says about a reversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReversalOutcome {
    Reversed,
    /// The reversal could not be confirmed; the attempt stays
    /// `REVERSAL_PENDING` and the sale cannot finalize.
    Unknown,
}

/// A payment processor, as the core sees it.
///
/// Implementations live outside the domain (a simulator here, a terminal or an
/// HTTP client later). Nothing in this trait may block on real time: the
/// application layer owns timeouts and the durable ordering around each call.
pub trait PaymentProviderPort: Send + Sync {
    fn kind(&self) -> ProviderKind;

    /// Charge. Called with no database transaction open, after the attempt is
    /// durably recorded as `PROCESSING` (ADR 020).
    fn start_payment(&self, request: &ProviderRequest) -> Result<ProviderOutcome, PaymentError>;

    /// Ask about an interaction already started. Must not charge.
    fn query_payment(&self, query: &ProviderQuery) -> Result<ProviderOutcome, PaymentError>;

    /// Undo an interaction. Idempotent on `correlation_id`.
    fn reverse_payment(&self, reversal: &ProviderReversal)
        -> Result<ReversalOutcome, PaymentError>;

    /// Give money back for a settled charge, for a commercial refund.
    ///
    /// Called with no database transaction open, after the refund attempt is
    /// durably recorded, exactly like a charge (ADR 020). Idempotent on the
    /// refund's `correlation_id`, so a provider-side retry cannot pay twice.
    fn refund_payment(&self, request: &RefundRequest) -> Result<RefundOutcome, PaymentError>;

    /// Ask what happened to a refund already sent. Must not refund.
    fn query_refund(&self, query: &ProviderQuery) -> Result<RefundOutcome, PaymentError>;
}
