//! What leaves the installation, and what comes back about it.
//!
//! Transport-neutral on purpose: there is no URL, no header, no status code and
//! no HTTP verb anywhere in this crate. The shapes below are what *any*
//! transport carries, and the one that exists today is a simulator (ADR 040).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One durable local fact, on its way out.
///
/// `event_id` is the **consumer's idempotency key** and never changes across
/// retries: that is what makes at-least-once safe (ADR 004). `seq` is the
/// installation's own durable order and is what forms batches deterministically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactEnvelope {
    pub event_id: Uuid,
    pub seq: i64,
    /// `SALE_FINALIZED`, `REFUND_COMPLETED`, … as its stable wire string.
    pub fact_type: String,
    pub aggregate_id: String,
    pub aggregate_version: i64,
    pub tenant_id: String,
    pub store_id: String,
    pub register_id: Option<String>,
    pub business_day_id: Option<String>,
    pub occurred_at: String,
    pub payload: serde_json::Value,
}

/// A batch is a unit of **transport**, never a unit of truth: every fact in it
/// is answered on its own (ADR 041).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundBatch {
    pub facts: Vec<FactEnvelope>,
}

impl OutboundBatch {
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.facts.len()
    }
}

/// What the remote says about one fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactOutcome {
    /// The consumer holds it. Durable there, done here.
    Acknowledged,
    /// Try again later. The fact returns to pending and waits out its backoff.
    TransientFailure { reason: String },
    /// It will never be accepted — malformed, or a contract the consumer
    /// rejects. Retrying forever would be a lie, so the fact becomes terminal
    /// and waits for a person (ADR 041).
    PermanentFailure { code: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactResult {
    pub event_id: Uuid,
    pub outcome: FactOutcome,
}

/// The answer to one batch: one result per fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundReceipt {
    pub results: Vec<FactResult>,
}

impl OutboundReceipt {
    /// Check the answer against what was actually sent.
    ///
    /// A receipt naming a fact nobody sent, or missing one that was, is a
    /// contract violation rather than a partial success: the whole exchange is
    /// discarded and retried, because applying half of an answer we cannot
    /// read is how state diverges (ADR 041).
    pub fn validate(&self, batch: &OutboundBatch) -> Result<(), crate::error::SyncError> {
        use crate::error::SyncError;
        if self.results.len() != batch.facts.len() {
            return Err(SyncError::Malformed {
                detail: format!(
                    "{} results for {} facts",
                    self.results.len(),
                    batch.facts.len()
                ),
            });
        }
        for result in &self.results {
            if !batch
                .facts
                .iter()
                .any(|fact| fact.event_id == result.event_id)
            {
                return Err(SyncError::Malformed {
                    detail: format!("a result for `{}`, which was not sent", result.event_id),
                });
            }
        }
        for fact in &batch.facts {
            if !self
                .results
                .iter()
                .any(|result| result.event_id == fact.event_id)
            {
                return Err(SyncError::Malformed {
                    detail: format!("no result for `{}`", fact.event_id),
                });
            }
        }
        Ok(())
    }
}

/// What kind of inbound message this is.
///
/// A **closed** set: an unknown kind makes the whole batch malformed rather
/// than something to skip, so a consumer that invents a message cannot leave
/// this installation half-updated (ADR 041).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InboundKind {
    /// Open a catalog import. Carries what the existing M3 lifecycle needs.
    CatalogBegin,
    /// One chunk of an open import.
    CatalogChunk,
    /// Close an import; the core verifies the content hash it computed.
    CatalogCommit,
    /// Put a committed snapshot in force, atomically.
    CatalogActivate,
    /// Project the fiscal state of a sale. **The only production write** to
    /// `transaction_fiscal_metadata`: PI.Edge consumes fiscal metadata and
    /// never declares a sale invoiced (ADR 032, amended).
    FiscalMetadata,
    /// Commercial settings of this installation. Hardware is never here
    /// (ADR 039, ADR 040).
    StoreSettings,
}

impl InboundKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            InboundKind::CatalogBegin => "CATALOG_BEGIN",
            InboundKind::CatalogChunk => "CATALOG_CHUNK",
            InboundKind::CatalogCommit => "CATALOG_COMMIT",
            InboundKind::CatalogActivate => "CATALOG_ACTIVATE",
            InboundKind::FiscalMetadata => "FISCAL_METADATA",
            InboundKind::StoreSettings => "STORE_SETTINGS",
        }
    }

    pub fn parse(value: &str) -> Result<Self, crate::error::SyncError> {
        match value {
            "CATALOG_BEGIN" => Ok(InboundKind::CatalogBegin),
            "CATALOG_CHUNK" => Ok(InboundKind::CatalogChunk),
            "CATALOG_COMMIT" => Ok(InboundKind::CatalogCommit),
            "CATALOG_ACTIVATE" => Ok(InboundKind::CatalogActivate),
            "FISCAL_METADATA" => Ok(InboundKind::FiscalMetadata),
            "STORE_SETTINGS" => Ok(InboundKind::StoreSettings),
            other => Err(crate::error::SyncError::Malformed {
                detail: format!("`{other}` is not an inbound message kind"),
            }),
        }
    }
}

/// One thing the remote wants this installation to apply.
///
/// `cursor` is the position *after* this message. It is written in the same
/// database transaction as the message's effect, so a cursor can never be
/// ahead of what is durable locally (ADR 041).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundMessage {
    pub message_id: Uuid,
    pub cursor: String,
    pub kind: InboundKind,
    /// Parsed by the application layer against the types that already exist.
    /// The sync engine carries it and does not understand it: there is exactly
    /// one catalog importer and this is not it (ADR 041).
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct InboundBatch {
    pub messages: Vec<InboundMessage>,
}

impl InboundBatch {
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// Ask the authority whether one sale may be refunded, now.
///
/// Deliberately **not** a durable grant: the answer authorises *this*
/// operation and nothing else, so a sale never becomes "refundable from now
/// on" (ADR 042).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundAuthorizationRequest {
    pub transaction_id: String,
    pub tenant_id: String,
    pub store_id: String,
    /// Minted per request, so a retry is recognisable as the same question.
    pub request_id: Uuid,
}

/// What the authority answered about this one operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefundAuthorizationDecision {
    /// Accepted centrally. `authorization_ref` is evidence to materialise on
    /// the refund — not a reusable permission.
    Accepted { authorization_ref: String },
    /// The authority exists and says no.
    Refused { reason: String },
}
