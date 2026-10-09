//! The payment provider port.
//!
//! Three capabilities, no more: start a payment, ask what happened to one,
//! and undo one. Everything crosses the boundary as domain types — never a
//! provider's JSON, never a raw terminal payload.
//!
//! There is deliberately **no refund method**. A refund returns money for a
//! completed sale; it is a different document and a different domain, and
//! putting it here would invite treating a reversal as a refund (ADR 019).

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
}
