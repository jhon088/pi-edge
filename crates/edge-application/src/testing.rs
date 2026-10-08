//! Deterministic ports for tests and golden cases.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use chrono::{DateTime, Duration, Utc};
use edge_domain::prelude::Timestamp;
use uuid::Uuid;

use crate::ports::{Clock, IdFactory};

/// Clock that starts at a fixed instant and advances by `step` on every read.
/// A `step` of zero freezes time, which is what golden cases use.
#[derive(Debug)]
pub struct FixedClock {
    start: DateTime<Utc>,
    step_millis: i64,
    reads: AtomicI64,
}

impl FixedClock {
    pub fn new(start: DateTime<Utc>, step_millis: i64) -> Self {
        Self {
            start,
            step_millis,
            reads: AtomicI64::new(0),
        }
    }

    pub fn frozen(start: DateTime<Utc>) -> Self {
        Self::new(start, 0)
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        let reads = self.reads.fetch_add(1, Ordering::SeqCst);
        let offset = Duration::milliseconds(self.step_millis.saturating_mul(reads));
        Timestamp::from_datetime(self.start + offset)
    }
}

/// Sequential UUID factory: `00000000-0000-0000-0000-0000000000NN`.
#[derive(Debug, Default)]
pub struct SequentialIdFactory {
    next: AtomicU64,
}

impl SequentialIdFactory {
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
        }
    }

    pub fn starting_at(value: u64) -> Self {
        Self {
            next: AtomicU64::new(value),
        }
    }
}

impl IdFactory for SequentialIdFactory {
    fn new_uuid(&self) -> Uuid {
        Uuid::from_u128(u128::from(self.next.fetch_add(1, Ordering::SeqCst)))
    }
}
