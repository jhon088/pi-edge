//! Sync use cases: push the facts out, pull the authority's messages in.
//!
//! The shape of a cycle, and why it is that shape:
//!
//! ```text
//! 1. claim     one transaction   PENDING -> SENDING, attempt counted
//! 2. send      no transaction    the transport is called
//! 3. settle    one transaction   SENT / PENDING+backoff / FAILED, per fact
//! 4. fetch     no transaction    ask from the stored cursor
//! 5. apply     one per message   the effect, then the cursor
//! ```
//!
//! Step 1 before step 2 is the rule payments and hardware already follow
//! (ADR 020): the attempt is durable *before* the remote is contacted, so a
//! process that dies in step 2 leaves evidence that the facts may already have
//! landed. The answer to that is not a lease or a heartbeat — one daemon owns
//! this database, so a `SENDING` row found at startup means "nobody is coming
//! back for it". [`EdgeApplication::recover_interrupted_sync`] puts those rows
//! back to `PENDING` and they are resent **with the same `event_id`**, which
//! the consumer deduplicates. At-least-once, deliberately (ADR 004, ADR 041).
//!
//! Step 5's order is the inbound half of the same idea: the effect commits
//! first and the cursor follows. A crash in between re-delivers the message,
//! and re-delivery is harmless because every inbound message is idempotent on
//! something the core already had — a catalog hash, a message id recorded in
//! `processed_commands`, or a projection whose second application writes the
//! same value. The forbidden order is the other one: a cursor ahead of the
//! local state would silently skip a message forever.
//!
//! What this file does **not** do: import a catalog. The inbound catalog
//! messages drive the existing milestone-3 lifecycle — begin, chunk, commit,
//! activate — and there is exactly one importer in PI.Edge (ADR 041).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use edge_catalog::{
    CatalogBarcode, CatalogProduct, Category, Channel, ChannelAvailability, PriceBook, PriceEntry,
    ProductPresentation, Promotion, SelectionGroup, SelectionOption, SnapshotCounts, TaxProfile,
};
use edge_domain::prelude::*;
use edge_payments::PaymentProviderPort;
use edge_refunds::{FiscalState, TransactionFiscalMetadata};
use edge_sync::{
    backoff_seconds, classify, form_batch, FactEnvelope, FactTransition, InboundKind,
    InboundMessage, OutboundBatch, SyncError, SyncTransport,
};
use serde::{Deserialize, Serialize};

use crate::commands::{
    ActivateCatalogSnapshotCommand, BeginCatalogImportCommand, CommitCatalogImportCommand,
    PutCatalogChunkCommand,
};
use crate::context::require_device;
use crate::error::AppError;
use crate::ports::{Clock, EdgeStore, IdFactory, UnitOfWork};
use crate::queries::{FailedFactView, SyncCycleOutcome, SyncCycleReport, SyncStatusView};
use crate::service::EdgeApplication;

/// The sync transport this installation talks through, and the limits it
/// talks within.
///
/// Composed at the composition root — the only place that knows whether there
/// is a server at all. Downstream code sees a port and a driver name, never a
/// host (ADR 040).
#[derive(Clone)]
pub struct SyncRuntime {
    pub transport: Arc<dyn SyncTransport>,
    /// Most facts in one batch.
    pub batch_limit: usize,
    /// Most inbound messages asked for at once.
    pub inbound_limit: usize,
}

impl std::fmt::Debug for SyncRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SyncRuntime")
            .field("transport", &self.transport.driver())
            .field("batch_limit", &self.batch_limit)
            .finish()
    }
}

impl SyncRuntime {
    /// **No sync configured.** What a core gets unless a composition root says
    /// otherwise.
    ///
    /// Not a degraded mode: an installation with no server to talk to is a
    /// completely valid PI.Edge. Facts accumulate durably and every sync
    /// operation answers `SYNC_NOT_CONFIGURED`. A simulator is never selected
    /// implicitly — same rule as hardware (ADR 039, ADR 040).
    pub fn unconfigured() -> Self {
        Self::with_transport(Arc::new(edge_sync::unconfigured::UnconfiguredSyncTransport))
    }

    pub fn with_transport(transport: Arc<dyn SyncTransport>) -> Self {
        Self {
            transport,
            batch_limit: edge_sync::DEFAULT_BATCH_LIMIT,
            inbound_limit: edge_sync::DEFAULT_BATCH_LIMIT,
        }
    }

    pub fn with_batch_limit(mut self, batch_limit: usize) -> Self {
        self.batch_limit = batch_limit.max(1);
        self
    }

    pub fn driver(&self) -> &'static str {
        self.transport.driver()
    }
}

impl Default for SyncRuntime {
    fn default() -> Self {
        Self::unconfigured()
    }
}

// ------------------------------------------------------- inbound message bodies
//
// The wire shapes of what the authority sends. They live here, next to the
// code that applies them, because applying them is all they are for: each one
// converts into a command the core already has.
//
// The catalog messages name the **upstream version**, not a local snapshot id.
// The server owns the version; PI.Edge mints its own snapshot id when the
// import opens, and resolves version -> snapshot locally. Neither side has to
// know the other's identifiers (ADR 041).

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundCatalogBegin {
    pub version: i64,
    pub currency: Currency,
    pub declared_content_hash: String,
    pub generated_at: Timestamp,
    #[serde(default)]
    pub effective_at: Option<Timestamp>,
    pub expected: SnapshotCounts,
    #[serde(default)]
    pub tax_profiles: Vec<TaxProfile>,
    #[serde(default)]
    pub price_books: Vec<PriceBook>,
    /// Universal catalog extensions (ADR 044). Defaulted, so a server that has
    /// never sent them keeps importing.
    #[serde(default)]
    pub channels: Vec<Channel>,
    #[serde(default)]
    pub categories: Vec<Category>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundCatalogChunk {
    pub version: i64,
    pub chunk_index: i64,
    pub chunk_hash: String,
    #[serde(default)]
    pub products: Vec<CatalogProduct>,
    #[serde(default)]
    pub price_entries: Vec<PriceEntry>,
    #[serde(default)]
    pub barcodes: Vec<CatalogBarcode>,
    #[serde(default)]
    pub promotions: Vec<Promotion>,
    #[serde(default)]
    pub presentation: Vec<ProductPresentation>,
    #[serde(default)]
    pub selection_groups: Vec<SelectionGroup>,
    #[serde(default)]
    pub selection_options: Vec<SelectionOption>,
    #[serde(default)]
    pub availability: Vec<ChannelAvailability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundCatalogCommit {
    pub version: i64,
    pub chunk_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundCatalogActivate {
    pub version: i64,
}

/// The fiscal state of one sale, as declared by the authority that issues
/// invoices.
///
/// **The only production write to `transaction_fiscal_metadata`.** PI.Edge
/// consumes fiscal metadata; it does not declare a sale invoiced, and there is
/// no local operation, protocol message, CLI command or UI control that does
/// (ADR 032, amended).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundFiscalMetadata {
    pub transaction_id: TransactionId,
    /// `NOT_INVOICED` or `INVOICED`: the stable code, not a Rust variant name.
    pub fiscal_state: String,
    #[serde(default)]
    pub invoice_ref: Option<String>,
}

impl InboundFiscalMetadata {
    /// The state this message declares.
    pub fn declared(&self) -> Result<FiscalState, AppError> {
        FiscalState::parse(&self.fiscal_state).map_err(|_| {
            AppError::Sync(SyncError::Malformed {
                detail: format!("`{}` is not a fiscal state", self.fiscal_state),
            })
        })
    }
}

/// Commercial settings of this installation.
///
/// One field, because one field has a real consumer. Hardware is **not** in
/// the inbound contract and never will be through this message: a printer
/// host, a drawer driver or a device topology is local to the composition root
/// (ADR 039, ADR 040). Neither is identity: tenant, store and currency are
/// provisioned, not synced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundStoreSettings {
    pub allow_negative_inventory: bool,
}

impl<S, C, I, P> EdgeApplication<S, C, I, P>
where
    S: EdgeStore,
    C: Clock,
    I: IdFactory,
    P: PaymentProviderPort,
{
    /// The transport this core syncs through, for diagnostics and for tests
    /// that assert on a simulator's journal.
    pub fn sync(&self) -> &SyncRuntime {
        &self.sync
    }

    /// Put every fact that was in flight back in the queue.
    ///
    /// Run once when the runtime starts, before any cycle. A `SENDING` row is
    /// not a resting state: this database has one writer, so a row still
    /// claimed after a restart was claimed by a process that no longer exists.
    /// It is resent with the same `event_id` (ADR 041).
    pub fn recover_interrupted_sync(&self) -> Result<i64, AppError> {
        let now = self.clock.now();
        let recovered = self
            .store()
            .transact(move |uow| uow.recover_sending_outbox(now))?;
        if recovered > 0 {
            tracing::warn!(
                recovered,
                "facts were in flight when the runtime stopped; they are queued again \
                 with the same event ids and the consumer will deduplicate"
            );
        }
        Ok(recovered)
    }

    /// One dispatch cycle: push what is due, then pull what is waiting.
    ///
    /// Never an error when the remote is simply absent. An unreachable or
    /// unconfigured server is a normal state for a till, reported in the
    /// outcome; the `Err` path is reserved for storage and serialization
    /// faults, which are bugs.
    pub fn run_sync_cycle(&self) -> Result<SyncCycleReport, AppError> {
        let mut report = SyncCycleReport {
            driver: self.sync.driver().to_owned(),
            outcome: SyncCycleOutcome::Idle,
            attempted: 0,
            acknowledged: 0,
            rescheduled: 0,
            failed: 0,
            inbound_applied: 0,
        };
        if let Some(error) = self.push_outbound(&mut report)? {
            report.outcome = outcome_for(&error);
            return Ok(report);
        }
        if let Some(error) = self.pull_inbound(&mut report)? {
            report.outcome = outcome_for(&error);
            return Ok(report);
        }
        if report.attempted > 0 || report.inbound_applied > 0 {
            report.outcome = SyncCycleOutcome::Progressed;
        }
        Ok(report)
    }

    /// What is waiting, what is stuck, and how far the inbound stream got.
    pub fn get_sync_status(&self) -> Result<SyncStatusView, AppError> {
        let driver = self.sync.driver().to_owned();
        self.store().transact(move |uow| {
            let counts = uow.outbox_counts()?;
            let pending = uow.list_outbox_by_status(OutboxStatus::Pending, 1)?;
            let failed = uow.list_outbox_by_status(OutboxStatus::Failed, 20)?;
            Ok(SyncStatusView {
                driver,
                outbox: counts,
                oldest_pending_at: pending.first().map(|entry| entry.event.occurred_at),
                next_attempt_at: pending
                    .first()
                    .and_then(|entry| entry.event.next_attempt_at),
                inbound_cursor: uow.inbound_cursor()?,
                failed: failed
                    .into_iter()
                    .map(|entry| FailedFactView {
                        seq: entry.seq,
                        event_id: entry.event.event_id,
                        event_type: entry.event.event_type,
                        aggregate_id: entry.event.aggregate_id,
                        attempt_count: entry.event.attempt_count,
                        error_code: entry.last_error_code,
                        error_detail: entry.last_error_detail,
                    })
                    .collect(),
            })
        })
    }

    // -------------------------------------------------------------- outbound

    /// Returns the transport failure, if the transport is what went wrong.
    fn push_outbound(&self, report: &mut SyncCycleReport) -> Result<Option<SyncError>, AppError> {
        let now = self.clock.now();
        let limit = self.sync.batch_limit;

        let ClaimedBatch { batch, attempts } = self
            .store()
            .transact(move |uow| claim_batch(uow, now, limit))?;
        if batch.is_empty() {
            return Ok(None);
        }
        report.attempted = batch.len() as i64;

        // No transaction is open while the remote is contacted.
        let settled = self
            .sync
            .transport
            .send_facts(&batch)
            // An answer we cannot read is not a partial success: nothing is
            // concluded from it and every fact is retried with its own id.
            .and_then(|receipt| receipt.validate(&batch).map(|()| receipt));

        match settled {
            Ok(receipt) => {
                let counted = self.store().transact(move |uow| {
                    let mut tally = (0, 0, 0);
                    for result in &receipt.results {
                        let event_id = EventId::from_uuid(result.event_id);
                        let attempt = attempts.get(&result.event_id).copied().unwrap_or(1);
                        match classify(&result.outcome, attempt) {
                            FactTransition::Sent => {
                                uow.mark_outbox_sent(event_id, now)?;
                                tally.0 += 1;
                            }
                            FactTransition::Retry {
                                after_seconds,
                                reason,
                            } => {
                                uow.reschedule_outbox(
                                    event_id,
                                    now.plus_seconds(after_seconds),
                                    "SYNC_TRANSIENT_FAILURE",
                                    &reason,
                                    now,
                                )?;
                                tally.1 += 1;
                            }
                            FactTransition::Failed { code, reason } => {
                                uow.fail_outbox(event_id, &code, &reason, now)?;
                                tally.2 += 1;
                            }
                        }
                    }
                    Ok(tally)
                })?;
                report.acknowledged = counted.0;
                report.rescheduled = counted.1;
                report.failed = counted.2;
                Ok(None)
            }
            Err(error) => {
                // The transport failed, so nothing is known fact by fact.
                // Every claimed fact goes back to pending with its backoff and
                // its original id. If the remote did receive the batch, the
                // resend is a duplicate it deduplicates — which is exactly
                // what at-least-once buys (ADR 004).
                let code = error.code();
                let detail = error.to_string();
                report.rescheduled = self.store().transact(move |uow| {
                    let mut rescheduled = 0;
                    for fact in &batch.facts {
                        let attempt = attempts.get(&fact.event_id).copied().unwrap_or(1);
                        uow.reschedule_outbox(
                            EventId::from_uuid(fact.event_id),
                            now.plus_seconds(backoff_seconds(attempt)),
                            code,
                            &detail,
                            now,
                        )?;
                        rescheduled += 1;
                    }
                    Ok(rescheduled)
                })?;
                Ok(Some(error))
            }
        }
    }

    // --------------------------------------------------------------- inbound

    fn pull_inbound(&self, report: &mut SyncCycleReport) -> Result<Option<SyncError>, AppError> {
        let limit = self.sync.inbound_limit;
        let cursor = self.store().transact(|uow| uow.inbound_cursor())?;
        let fetched = match self.sync.transport.fetch_inbound(cursor.as_deref(), limit) {
            Ok(batch) => batch,
            Err(error) => return Ok(Some(error)),
        };

        for message in fetched.messages {
            // Strictly in order, and the first message that cannot be applied
            // stops the cycle. Skipping one would advance the cursor past an
            // effect that never happened.
            match self.apply_inbound(&message) {
                Ok(()) => report.inbound_applied += 1,
                Err(AppError::Sync(error)) => return Ok(Some(error)),
                Err(other) => return Err(other),
            }
        }
        Ok(None)
    }

    /// Apply one message, then record that it was applied.
    fn apply_inbound(&self, message: &InboundMessage) -> Result<(), AppError> {
        let now = self.clock.now();
        match message.kind {
            // The catalog path drives the existing import lifecycle, whose
            // calls each own a transaction. The cursor therefore follows in a
            // second transaction — never precedes. A crash in between
            // re-delivers the message, and the import's own idempotency (the
            // content hash, the chunk hash, the recorded `message_id`) makes
            // the second application a no-op.
            InboundKind::CatalogBegin => {
                let body: InboundCatalogBegin = parse_body(message)?;
                self.begin_catalog_import(BeginCatalogImportCommand {
                    request_id: Some(message.message_id),
                    version: body.version,
                    currency: body.currency,
                    declared_content_hash: body.declared_content_hash,
                    generated_at: body.generated_at,
                    effective_at: body.effective_at,
                    expected: body.expected,
                    tax_profiles: body.tax_profiles,
                    price_books: body.price_books,
                    channels: body.channels,
                    categories: body.categories,
                })?;
            }
            InboundKind::CatalogChunk => {
                let body: InboundCatalogChunk = parse_body(message)?;
                let snapshot_id = self.resolve_snapshot(body.version)?;
                self.put_catalog_chunk(PutCatalogChunkCommand {
                    snapshot_id,
                    chunk_index: body.chunk_index,
                    chunk_hash: body.chunk_hash,
                    products: body.products,
                    price_entries: body.price_entries,
                    barcodes: body.barcodes,
                    promotions: body.promotions,
                    presentation: body.presentation,
                    selection_groups: body.selection_groups,
                    selection_options: body.selection_options,
                    availability: body.availability,
                })?;
            }
            InboundKind::CatalogCommit => {
                let body: InboundCatalogCommit = parse_body(message)?;
                let snapshot_id = self.resolve_snapshot(body.version)?;
                self.commit_catalog_import(CommitCatalogImportCommand {
                    request_id: Some(message.message_id),
                    snapshot_id,
                    chunk_count: body.chunk_count,
                })?;
            }
            InboundKind::CatalogActivate => {
                let body: InboundCatalogActivate = parse_body(message)?;
                let snapshot_id = self.resolve_snapshot(body.version)?;
                self.activate_catalog_snapshot(ActivateCatalogSnapshotCommand {
                    request_id: Some(message.message_id),
                    snapshot_id,
                })?;
            }
            // Projections write through the unit of work directly, so the
            // effect and the cursor share one transaction: the strongest form
            // of the rule.
            InboundKind::FiscalMetadata => {
                let body: InboundFiscalMetadata = parse_body(message)?;
                let cursor = message.cursor.clone();
                return self.store().transact(move |uow| {
                    apply_fiscal_metadata(uow, &body, now)?;
                    uow.put_inbound_cursor(&cursor, now)
                });
            }
            InboundKind::StoreSettings => {
                let body: InboundStoreSettings = parse_body(message)?;
                let cursor = message.cursor.clone();
                return self.store().transact(move |uow| {
                    let mut profile = require_device(uow)?;
                    profile.allow_negative_inventory = body.allow_negative_inventory;
                    uow.put_device_profile(&profile, now)?;
                    uow.put_inbound_cursor(&cursor, now)
                });
            }
        }
        let cursor = message.cursor.clone();
        self.store()
            .transact(move |uow| uow.put_inbound_cursor(&cursor, now))
    }

    /// Which local snapshot an upstream version opened.
    fn resolve_snapshot(&self, version: i64) -> Result<CatalogSnapshotId, AppError> {
        self.store().transact(move |uow| {
            let device = require_device(uow)?;
            match uow.find_catalog_snapshot_by_version(&device.store_id, version)? {
                Some(snapshot) => Ok(snapshot.id()),
                None => Err(AppError::Sync(SyncError::Malformed {
                    detail: format!(
                        "catalog version {version} has no open import on this installation; \
                         the begin message has not been applied"
                    ),
                })),
            }
        })
    }
}

// ----------------------------------------------------------------- internals

/// A claimed batch and the attempt count each of its facts now carries.
///
/// The counts travel beside the batch rather than inside `FactEnvelope`: how
/// many times PI.Edge has tried is local dispatch state, and the consumer has
/// no business receiving it.
struct ClaimedBatch {
    batch: OutboundBatch,
    attempts: HashMap<uuid::Uuid, i64>,
}

/// Read the pending facts, form the batch, and claim it.
///
/// The window read is wider than the batch limit on purpose: `form_batch`
/// drops later versions of an aggregate and anything blocked behind a terminal
/// fact, so reading exactly `limit` rows could form an almost empty batch
/// while unrelated work waits. It is still bounded — a till's outbox is never
/// read whole.
fn claim_batch(
    uow: &mut dyn UnitOfWork,
    now: Timestamp,
    limit: usize,
) -> Result<ClaimedBatch, AppError> {
    let blocked: HashSet<String> = uow.list_failed_outbox_aggregates()?.into_iter().collect();
    let window = limit.saturating_mul(4) as i64;
    let pending = uow.list_dispatchable_outbox(now, window)?;
    let envelopes: Vec<FactEnvelope> = pending.iter().map(envelope_of).collect();

    let batch = form_batch(&envelopes, limit, &blocked);
    let ids: Vec<EventId> = batch
        .facts
        .iter()
        .map(|fact| EventId::from_uuid(fact.event_id))
        .collect();
    let claimed: HashSet<EventId> = uow.claim_outbox(&ids, now)?.into_iter().collect();

    // `claim_outbox` incremented the attempt count, so the value the backoff
    // needs is one past what was read.
    let mut attempts = HashMap::new();
    for entry in &pending {
        attempts.insert(
            entry.event.event_id.to_uuid(),
            entry.event.attempt_count + 1,
        );
    }
    Ok(ClaimedBatch {
        batch: OutboundBatch {
            facts: batch
                .facts
                .into_iter()
                .filter(|fact| claimed.contains(&EventId::from_uuid(fact.event_id)))
                .collect(),
        },
        attempts,
    })
}

fn envelope_of(entry: &OutboxEntry) -> FactEnvelope {
    FactEnvelope {
        event_id: entry.event.event_id.to_uuid(),
        seq: entry.seq,
        fact_type: entry.event.event_type.as_str().to_owned(),
        aggregate_id: entry.event.aggregate_id.clone(),
        aggregate_version: entry.event.aggregate_version,
        tenant_id: entry.event.tenant_id.as_str().to_owned(),
        store_id: entry.event.store_id.as_str().to_owned(),
        register_id: entry
            .event
            .register_id
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        business_day_id: entry.event.business_day_id.map(|id| id.to_string()),
        occurred_at: entry.event.occurred_at.to_rfc3339(),
        payload: entry.event.payload_json.clone(),
    }
}

/// Fiscal state arrives from outside and only ever moves forward.
///
/// `NOT_INVOICED -> INVOICED` is applied; the reverse is not. An authority
/// that re-sends `NOT_INVOICED` for a sale it already invoiced must not be
/// able to rewrite that history through this projection (ADR 032, amended).
pub(crate) fn apply_fiscal_metadata(
    uow: &mut dyn UnitOfWork,
    body: &InboundFiscalMetadata,
    now: Timestamp,
) -> Result<(), AppError> {
    crate::refund_service::require_finalized(uow, body.transaction_id)?;
    if body.declared()? != FiscalState::Invoiced {
        // Every finalized sale already carries `NOT_INVOICED`, written at
        // finalization. There is nothing to apply and nothing to complain
        // about.
        return Ok(());
    }
    let invoice_ref = body.invoice_ref.as_deref().unwrap_or("").trim().to_owned();
    if invoice_ref.is_empty() {
        return Err(AppError::Sync(SyncError::Malformed {
            detail: "an invoiced sale needs an invoice reference".to_owned(),
        }));
    }
    uow.put_transaction_fiscal_metadata(
        body.transaction_id,
        &TransactionFiscalMetadata {
            fiscal_state: FiscalState::Invoiced,
            invoice_ref: Some(invoice_ref),
            updated_at: now,
        },
    )
}

fn parse_body<T: serde::de::DeserializeOwned>(message: &InboundMessage) -> Result<T, AppError> {
    serde_json::from_value(message.payload.clone()).map_err(|error| {
        AppError::Sync(SyncError::Malformed {
            detail: format!("{} payload: {error}", message.kind.as_str()),
        })
    })
}

fn outcome_for(error: &SyncError) -> SyncCycleOutcome {
    match error {
        SyncError::NotConfigured => SyncCycleOutcome::NotConfigured,
        other => SyncCycleOutcome::TransportFailed {
            code: other.code().to_owned(),
            detail: other.to_string(),
        },
    }
}
