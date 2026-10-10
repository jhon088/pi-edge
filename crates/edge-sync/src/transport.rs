//! The port a real server will one day sit behind.
//!
//! Three operations, no transport vocabulary: push facts out, pull messages in,
//! ask the authority about one refund. There is no URL, no authentication, no
//! retry and no connection here — retry is the engine's policy, and the
//! connection is the adapter's problem (ADR 040).

use crate::error::SyncError;
use crate::fact::{
    InboundBatch, OutboundBatch, OutboundReceipt, RefundAuthorizationDecision,
    RefundAuthorizationRequest,
};

pub trait SyncTransport: Send + Sync + std::fmt::Debug {
    /// Which adapter this is, for diagnostics. Never a host, never a port.
    fn driver(&self) -> &'static str;

    /// Hand a batch to the remote and return one result per fact.
    ///
    /// An `Err` here says nothing about the facts: a `Timeout` in particular
    /// means the remote may hold them, and the engine answers that by sending
    /// the same `event_id`s again.
    fn send_facts(&self, batch: &OutboundBatch) -> Result<OutboundReceipt, SyncError>;

    /// Ask for whatever the remote has after `cursor`.
    fn fetch_inbound(&self, cursor: Option<&str>, limit: usize) -> Result<InboundBatch, SyncError>;

    /// Ask whether this sale may be refunded, now.
    ///
    /// The answer authorises one operation. It is never stored as a standing
    /// permission (ADR 042).
    fn authorize_refund(
        &self,
        request: &RefundAuthorizationRequest,
    ) -> Result<RefundAuthorizationDecision, SyncError>;
}
