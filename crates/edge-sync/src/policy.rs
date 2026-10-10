//! The decisions the sync engine makes, as pure functions.
//!
//! Nothing here touches a database, a socket or a clock. A batch is formed from
//! a list; a backoff is computed from an attempt count; an answer is classified
//! into a next state. That is deliberate: these are the rules worth testing
//! exhaustively, and they are testable exhaustively only while they stay pure.

use std::collections::HashSet;

use crate::fact::{FactEnvelope, FactOutcome, OutboundBatch};

/// How many facts travel together at most.
///
/// A batch is a transport unit. The limit exists so one offline week does not
/// become one enormous request, not because the number means anything.
pub const DEFAULT_BATCH_LIMIT: usize = 50;

/// First retry delay, and the doubling cap.
const BACKOFF_BASE_SECONDS: i64 = 1;
const BACKOFF_CEILING_SECONDS: i64 = 300;

/// Where a fact goes next, decided from one remote answer.
///
/// This is the whole state machine for an outbound fact, and it is a function
/// of the answer alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactTransition {
    /// `SENDING -> SENT`.
    Sent,
    /// `SENDING -> PENDING`, attempt counted, next attempt pushed out.
    Retry { after_seconds: i64, reason: String },
    /// `SENDING -> FAILED`. Terminal, visible, never retried automatically and
    /// never deleted (ADR 041).
    Failed { code: String, reason: String },
}

/// Classify one remote answer for a fact that has already been attempted
/// `attempt_count` times (counting this attempt).
pub fn classify(outcome: &FactOutcome, attempt_count: i64) -> FactTransition {
    match outcome {
        FactOutcome::Acknowledged => FactTransition::Sent,
        FactOutcome::TransientFailure { reason } => FactTransition::Retry {
            after_seconds: backoff_seconds(attempt_count),
            reason: reason.clone(),
        },
        FactOutcome::PermanentFailure { code, reason } => FactTransition::Failed {
            code: code.clone(),
            reason: reason.clone(),
        },
    }
}

/// Deterministic exponential backoff: 1s, 2s, 4s … capped at five minutes.
///
/// No jitter. One till talking to one server has no thundering herd to spread,
/// and randomness here would only make the retry schedule untestable.
pub fn backoff_seconds(attempt_count: i64) -> i64 {
    if attempt_count <= 1 {
        return BACKOFF_BASE_SECONDS;
    }
    // Shift rather than `pow`, and stop shifting well before i64 overflow.
    let exponent = (attempt_count - 1).min(40) as u32;
    let delay = BACKOFF_BASE_SECONDS.saturating_mul(1_i64 << exponent);
    delay.min(BACKOFF_CEILING_SECONDS)
}

/// Pick the next batch out of the pending facts, in `seq` order.
///
/// Two rules, and no more than two:
///
/// 1. **`seq` is the order.** It is the installation's own durable sequence, so
///    two runs over the same pending set form the same batch.
/// 2. **At most one fact per aggregate.** Version `n+1` of an aggregate is
///    never in flight before version `n` is acknowledged — the lowest `seq`
///    for an aggregate goes now, the rest wait for the next cycle. That is the
///    whole ordering guarantee, built from metadata the outbox already carries.
///    There is no dependency graph here and there should never be one.
///
/// `blocked_aggregates` holds the aggregates that have a `FAILED` fact. Those
/// are skipped, because sending a later version of an aggregate whose earlier
/// version was permanently rejected would hand the consumer a history with a
/// hole in it. Every *other* aggregate keeps flowing: one terminal fact must
/// never stop the till's unrelated sales from syncing (ADR 041).
pub fn form_batch(
    pending: &[FactEnvelope],
    limit: usize,
    blocked_aggregates: &HashSet<String>,
) -> OutboundBatch {
    let mut ordered: Vec<&FactEnvelope> = pending.iter().collect();
    ordered.sort_by_key(|fact| fact.seq);

    let mut claimed: HashSet<&str> = HashSet::new();
    let mut facts = Vec::new();
    for fact in ordered {
        if facts.len() >= limit {
            break;
        }
        if blocked_aggregates.contains(&fact.aggregate_id) {
            continue;
        }
        if !claimed.insert(fact.aggregate_id.as_str()) {
            continue;
        }
        facts.push(fact.clone());
    }
    OutboundBatch { facts }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn fact(seq: i64, aggregate: &str, version: i64) -> FactEnvelope {
        FactEnvelope {
            event_id: Uuid::from_u128(seq as u128),
            seq,
            fact_type: "SALE_FINALIZED".to_owned(),
            aggregate_id: aggregate.to_owned(),
            aggregate_version: version,
            tenant_id: "tenant-1".to_owned(),
            store_id: "store-1".to_owned(),
            register_id: Some("reg-1".to_owned()),
            business_day_id: None,
            occurred_at: "2026-01-01T00:00:00Z".to_owned(),
            payload: serde_json::json!({}),
        }
    }

    #[test]
    fn a_batch_follows_seq_order_whatever_order_it_was_given() {
        let pending = vec![fact(3, "c", 1), fact(1, "a", 1), fact(2, "b", 1)];
        let batch = form_batch(&pending, 10, &HashSet::new());
        let seqs: Vec<i64> = batch.facts.iter().map(|fact| fact.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3]);
    }

    #[test]
    fn a_later_version_of_an_aggregate_waits_for_the_next_cycle() {
        // Three versions of one sale plus an unrelated one. Only the oldest
        // version of the sale travels; the unrelated fact is not held back.
        let pending = vec![
            fact(1, "sale-1", 1),
            fact(2, "sale-1", 2),
            fact(3, "sale-1", 3),
            fact(4, "sale-2", 1),
        ];
        let batch = form_batch(&pending, 10, &HashSet::new());
        let seqs: Vec<i64> = batch.facts.iter().map(|fact| fact.seq).collect();
        assert_eq!(seqs, vec![1, 4]);
    }

    #[test]
    fn a_failed_aggregate_is_skipped_and_the_rest_keep_flowing() {
        let pending = vec![fact(1, "sale-1", 2), fact(2, "sale-2", 1)];
        let blocked = HashSet::from(["sale-1".to_owned()]);
        let batch = form_batch(&pending, 10, &blocked);
        let seqs: Vec<i64> = batch.facts.iter().map(|fact| fact.seq).collect();
        assert_eq!(seqs, vec![2]);
    }

    #[test]
    fn the_limit_caps_the_batch_and_keeps_the_oldest() {
        let pending: Vec<FactEnvelope> = (1..=10)
            .map(|seq| fact(seq, &format!("sale-{seq}"), 1))
            .collect();
        let batch = form_batch(&pending, 3, &HashSet::new());
        let seqs: Vec<i64> = batch.facts.iter().map(|fact| fact.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3]);
    }

    #[test]
    fn backoff_doubles_then_settles_at_the_ceiling() {
        assert_eq!(backoff_seconds(0), 1);
        assert_eq!(backoff_seconds(1), 1);
        assert_eq!(backoff_seconds(2), 2);
        assert_eq!(backoff_seconds(3), 4);
        assert_eq!(backoff_seconds(9), 256);
        assert_eq!(backoff_seconds(10), 300);
        assert_eq!(backoff_seconds(1_000), 300);
    }

    #[test]
    fn each_outcome_maps_to_exactly_one_next_state() {
        assert_eq!(
            classify(&FactOutcome::Acknowledged, 1),
            FactTransition::Sent
        );
        assert_eq!(
            classify(
                &FactOutcome::TransientFailure {
                    reason: "busy".to_owned()
                },
                3
            ),
            FactTransition::Retry {
                after_seconds: 4,
                reason: "busy".to_owned()
            }
        );
        assert_eq!(
            classify(
                &FactOutcome::PermanentFailure {
                    code: "CONTRACT_REJECTED".to_owned(),
                    reason: "unknown fact type".to_owned()
                },
                1
            ),
            FactTransition::Failed {
                code: "CONTRACT_REJECTED".to_owned(),
                reason: "unknown fact type".to_owned()
            }
        );
    }
}
