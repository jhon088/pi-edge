//! Read models returned by the query use cases.
//!
//! These are plain application-level views. Wire formatting lives in
//! `edge-protocol`.

use edge_domain::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ports::DeviceProfile;

/// Everything a client needs to render the POS after a cold start.
/// Rebuilt entirely from SQLite: nothing lives only in memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeState {
    pub core_version: String,
    pub device: Option<DeviceProfile>,
    pub business_day: Option<BusinessDay>,
    pub session: Option<RegisterSession>,
    pub active_transaction: Option<RetailTransaction>,
    pub pending_outbox: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOutbox {
    pub pending_count: i64,
    pub events: Vec<OutboxEvent>,
}
