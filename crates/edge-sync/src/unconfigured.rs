//! What sync is when nobody configured it: nothing, loudly.
//!
//! This is the production default, and it is not a degraded mode — an
//! installation with no server to talk to is a completely valid PI.Edge. Facts
//! accumulate durably, the till keeps selling, and every sync operation
//! answers `SYNC_NOT_CONFIGURED`.
//!
//! Same rule as hardware (ADR 039): a missing configuration never selects a
//! simulator.

use crate::error::SyncError;
use crate::fact::{
    InboundBatch, OutboundBatch, OutboundReceipt, RefundAuthorizationDecision,
    RefundAuthorizationRequest,
};
use crate::transport::SyncTransport;

pub const UNCONFIGURED: &str = "unconfigured";

#[derive(Debug, Default)]
pub struct UnconfiguredSyncTransport;

impl SyncTransport for UnconfiguredSyncTransport {
    fn driver(&self) -> &'static str {
        UNCONFIGURED
    }

    fn send_facts(&self, _batch: &OutboundBatch) -> Result<OutboundReceipt, SyncError> {
        Err(SyncError::NotConfigured)
    }

    fn fetch_inbound(
        &self,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<InboundBatch, SyncError> {
        Err(SyncError::NotConfigured)
    }

    fn authorize_refund(
        &self,
        _request: &RefundAuthorizationRequest,
    ) -> Result<RefundAuthorizationDecision, SyncError> {
        Err(SyncError::NotConfigured)
    }
}
