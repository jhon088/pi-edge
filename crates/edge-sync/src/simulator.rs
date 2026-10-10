//! A scripted transport, for proving the engine's behaviour without a server.
//!
//! It is **not** a fake PI.COMMERCE Server. It implements none of the future
//! server's business rules: it does not price anything, validate a sale, keep a
//! catalog or decide a refund on merit. All it does is accept batches, answer
//! them the way the script says, remember which `event_id`s it has seen, and
//! hand over inbound messages that were put there by the test (ADR 040).
//!
//! Remembering the `event_id`s is the point: that is how "delivered twice,
//! applied once" becomes something a test can assert.
//!
//! **Opt-in only.** Nothing constructs this unless an operator asked for it by
//! name.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use uuid::Uuid;

use crate::error::SyncError;
use crate::fact::{
    FactOutcome, FactResult, InboundBatch, InboundMessage, OutboundBatch, OutboundReceipt,
    RefundAuthorizationDecision, RefundAuthorizationRequest,
};
use crate::transport::SyncTransport;

pub const SIMULATOR: &str = "simulator";

/// What the simulator does with the next batch it is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimulatorBehavior {
    /// Receive and acknowledge everything.
    AcknowledgeAll,
    /// Receive everything, then answer every fact "not now".
    TransientAll { reason: String },
    /// Receive everything, then reject every fact for good.
    PermanentAll { code: String, reason: String },
    /// Acknowledge the batch except for the named facts, which get a transient
    /// failure. The partial answer a real server will give.
    TransientFor {
        event_ids: Vec<Uuid>,
        reason: String,
    },
    /// **Receive the batch and then time out.** The dangerous case: the remote
    /// holds the facts, PI.Edge never learns it.
    ReceiveThenTimeout { elapsed_ms: u64 },
    /// Never receive anything — the batch does not leave.
    Unavailable { detail: String },
    /// Receive the batch and answer something unreadable.
    MalformedReceipt,
}

/// A simulator with nothing scripted acknowledges. Explicit failure is what a
/// test has to ask for, so a test that forgets to script is not silently
/// testing the sad path.
#[derive(Debug, Default)]
struct Script {
    outbound: VecDeque<SimulatorBehavior>,
    inbound: VecDeque<InboundMessage>,
    inbound_failures: VecDeque<SyncError>,
    refund_decisions: VecDeque<Result<RefundAuthorizationDecision, SyncError>>,
}

#[derive(Debug, Default)]
struct Journal {
    /// Every `event_id` the simulator received, in order, duplicates included.
    deliveries: Vec<Uuid>,
    /// How many times each `event_id` arrived.
    delivery_count: HashMap<Uuid, usize>,
    /// Each batch as it was received.
    batches: Vec<Vec<Uuid>>,
    /// Cursors the engine asked from.
    inbound_requests: Vec<Option<String>>,
    refund_requests: Vec<RefundAuthorizationRequest>,
}

#[derive(Debug, Default)]
pub struct SimulatorSyncTransport {
    script: Mutex<Script>,
    journal: Mutex<Journal>,
}

impl SimulatorSyncTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue what happens to the next batch (and the next, and the next).
    pub fn script_outbound(&self, behaviors: impl IntoIterator<Item = SimulatorBehavior>) {
        let mut script = self.lock_script();
        script.outbound.extend(behaviors);
    }

    /// Queue messages the remote will hand over.
    pub fn script_inbound(&self, messages: impl IntoIterator<Item = InboundMessage>) {
        let mut script = self.lock_script();
        script.inbound.extend(messages);
    }

    /// Queue a failure for the next inbound fetch.
    pub fn script_inbound_failure(&self, error: SyncError) {
        let mut script = self.lock_script();
        script.inbound_failures.push_back(error);
    }

    /// Queue what the authority answers about the next refund question.
    pub fn script_refund_decision(&self, decision: Result<RefundAuthorizationDecision, SyncError>) {
        let mut script = self.lock_script();
        script.refund_decisions.push_back(decision);
    }

    /// How many times this fact was delivered. More than one means a replay
    /// happened — which is allowed, as long as the effect happened once.
    pub fn delivery_count(&self, event_id: Uuid) -> usize {
        self.lock_journal()
            .delivery_count
            .get(&event_id)
            .copied()
            .unwrap_or(0)
    }

    /// Distinct facts the simulator holds.
    pub fn distinct_facts_received(&self) -> usize {
        self.lock_journal().delivery_count.len()
    }

    /// Every delivery in order, duplicates included.
    pub fn deliveries(&self) -> Vec<Uuid> {
        self.lock_journal().deliveries.clone()
    }

    /// Each received batch, in order.
    pub fn batches(&self) -> Vec<Vec<Uuid>> {
        self.lock_journal().batches.clone()
    }

    pub fn inbound_requests(&self) -> Vec<Option<String>> {
        self.lock_journal().inbound_requests.clone()
    }

    pub fn refund_requests(&self) -> Vec<RefundAuthorizationRequest> {
        self.lock_journal().refund_requests.clone()
    }

    // `Mutex` poisoning only happens if a holder panicked, and nothing here
    // panics while holding one. Recovering the guard keeps the simulator from
    // turning a test failure elsewhere into an unrelated panic here, and keeps
    // this crate free of `unwrap`.
    fn lock_script(&self) -> std::sync::MutexGuard<'_, Script> {
        self.script
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_journal(&self) -> std::sync::MutexGuard<'_, Journal> {
        self.journal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn record_delivery(&self, batch: &OutboundBatch) {
        let mut journal = self.lock_journal();
        let ids: Vec<Uuid> = batch.facts.iter().map(|fact| fact.event_id).collect();
        for id in &ids {
            journal.deliveries.push(*id);
            *journal.delivery_count.entry(*id).or_insert(0) += 1;
        }
        journal.batches.push(ids);
    }

    fn receipt(
        batch: &OutboundBatch,
        outcome_for: impl Fn(Uuid) -> FactOutcome,
    ) -> OutboundReceipt {
        OutboundReceipt {
            results: batch
                .facts
                .iter()
                .map(|fact| FactResult {
                    event_id: fact.event_id,
                    outcome: outcome_for(fact.event_id),
                })
                .collect(),
        }
    }
}

impl SyncTransport for SimulatorSyncTransport {
    fn driver(&self) -> &'static str {
        SIMULATOR
    }

    fn send_facts(&self, batch: &OutboundBatch) -> Result<OutboundReceipt, SyncError> {
        let behavior = {
            let mut script = self.lock_script();
            script
                .outbound
                .pop_front()
                .unwrap_or(SimulatorBehavior::AcknowledgeAll)
        };

        // `Unavailable` is the only behaviour where the batch never arrives.
        // Every other one records the delivery *before* answering, because
        // that is what makes a timeout genuinely ambiguous.
        if !matches!(behavior, SimulatorBehavior::Unavailable { .. }) {
            self.record_delivery(batch);
        }

        match behavior {
            SimulatorBehavior::Unavailable { detail } => Err(SyncError::Unavailable { detail }),
            SimulatorBehavior::AcknowledgeAll => {
                Ok(Self::receipt(batch, |_| FactOutcome::Acknowledged))
            }
            SimulatorBehavior::TransientAll { reason } => {
                Ok(Self::receipt(batch, |_| FactOutcome::TransientFailure {
                    reason: reason.clone(),
                }))
            }
            SimulatorBehavior::PermanentAll { code, reason } => {
                Ok(Self::receipt(batch, |_| FactOutcome::PermanentFailure {
                    code: code.clone(),
                    reason: reason.clone(),
                }))
            }
            SimulatorBehavior::TransientFor { event_ids, reason } => {
                Ok(Self::receipt(batch, |event_id| {
                    if event_ids.contains(&event_id) {
                        FactOutcome::TransientFailure {
                            reason: reason.clone(),
                        }
                    } else {
                        FactOutcome::Acknowledged
                    }
                }))
            }
            SimulatorBehavior::ReceiveThenTimeout { elapsed_ms } => {
                Err(SyncError::Timeout { elapsed_ms })
            }
            SimulatorBehavior::MalformedReceipt => Ok(OutboundReceipt {
                // One result too few: enough for `validate` to refuse the whole
                // exchange.
                results: Vec::new(),
            }),
        }
    }

    fn fetch_inbound(&self, cursor: Option<&str>, limit: usize) -> Result<InboundBatch, SyncError> {
        {
            let mut journal = self.lock_journal();
            journal.inbound_requests.push(cursor.map(str::to_owned));
        }
        let mut script = self.lock_script();
        if let Some(error) = script.inbound_failures.pop_front() {
            return Err(error);
        }
        let mut messages = Vec::new();
        while messages.len() < limit {
            match script.inbound.pop_front() {
                Some(message) => messages.push(message),
                None => break,
            }
        }
        Ok(InboundBatch { messages })
    }

    fn authorize_refund(
        &self,
        request: &RefundAuthorizationRequest,
    ) -> Result<RefundAuthorizationDecision, SyncError> {
        {
            let mut journal = self.lock_journal();
            journal.refund_requests.push(request.clone());
        }
        let mut script = self.lock_script();
        match script.refund_decisions.pop_front() {
            Some(decision) => decision,
            // Nothing scripted: the authority is reachable and says yes. A test
            // that wants a refusal scripts one.
            None => Ok(RefundAuthorizationDecision::Accepted {
                authorization_ref: format!("SIM-AUTH:{}", request.transaction_id),
            }),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::fact::FactEnvelope;

    fn batch(ids: &[u128]) -> OutboundBatch {
        OutboundBatch {
            facts: ids
                .iter()
                .enumerate()
                .map(|(index, id)| FactEnvelope {
                    event_id: Uuid::from_u128(*id),
                    seq: index as i64 + 1,
                    fact_type: "SALE_FINALIZED".to_owned(),
                    aggregate_id: format!("sale-{id}"),
                    aggregate_version: 1,
                    tenant_id: "tenant-1".to_owned(),
                    store_id: "store-1".to_owned(),
                    register_id: None,
                    business_day_id: None,
                    occurred_at: "2026-01-01T00:00:00Z".to_owned(),
                    payload: serde_json::json!({}),
                })
                .collect(),
        }
    }

    #[test]
    fn a_timeout_still_means_the_remote_received_the_batch() {
        let transport = SimulatorSyncTransport::new();
        transport.script_outbound([SimulatorBehavior::ReceiveThenTimeout { elapsed_ms: 30_000 }]);
        let error = transport
            .send_facts(&batch(&[1, 2]))
            .expect_err("the simulator was scripted to time out");
        assert_eq!(error.code(), "SYNC_TIMEOUT");
        assert!(error.may_have_landed());
        assert_eq!(transport.distinct_facts_received(), 2);
    }

    #[test]
    fn an_unavailable_transport_receives_nothing() {
        let transport = SimulatorSyncTransport::new();
        transport.script_outbound([SimulatorBehavior::Unavailable {
            detail: "no route".to_owned(),
        }]);
        let error = transport
            .send_facts(&batch(&[1]))
            .expect_err("the simulator was scripted as unavailable");
        assert!(!error.may_have_landed());
        assert_eq!(transport.distinct_facts_received(), 0);
    }

    #[test]
    fn a_replayed_fact_is_delivered_twice_and_acknowledged_twice() {
        let transport = SimulatorSyncTransport::new();
        let one = batch(&[7]);
        assert!(transport.send_facts(&one).is_ok());
        assert!(transport.send_facts(&one).is_ok());
        assert_eq!(transport.delivery_count(Uuid::from_u128(7)), 2);
        assert_eq!(transport.distinct_facts_received(), 1);
    }

    #[test]
    fn a_malformed_receipt_is_caught_by_validation() {
        let transport = SimulatorSyncTransport::new();
        transport.script_outbound([SimulatorBehavior::MalformedReceipt]);
        let sent = batch(&[1, 2]);
        let receipt = transport
            .send_facts(&sent)
            .expect("the simulator answered, just badly");
        let error = receipt
            .validate(&sent)
            .expect_err("a receipt with no results cannot be applied");
        assert_eq!(error.code(), "SYNC_MALFORMED_RESPONSE");
    }
}
