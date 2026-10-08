//! Minimal built-in port implementations that carry no external dependency.

use chrono::Utc;
use edge_domain::prelude::Timestamp;
use uuid::Uuid;

use crate::ports::{Clock, IdFactory};

/// Wall-clock time in UTC.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_datetime(Utc::now())
    }
}

/// Random UUID v4 factory for public ids.
#[derive(Debug, Default, Clone, Copy)]
pub struct UuidFactory;

impl IdFactory for UuidFactory {
    fn new_uuid(&self) -> Uuid {
        Uuid::new_v4()
    }
}
