//! Sync policy and the transport contract for PI.Edge.
//!
//! PI.Edge sells with or without a server. Facts about what happened are
//! written durably by the commercial domain, which knows nothing about sync;
//! this crate decides which of them travel, in what order, and what a remote
//! answer means. It owns no database, no socket and no HTTP — persistence is
//! `edge-storage`'s, orchestration is `edge-application`'s, and the only
//! transport that exists today is a simulator (ADR 040).
//!
//! ```text
//! domain facts ─> outbox (edge-storage) ─> edge-sync policy ─> SyncTransport
//!                                                               ├─ Unconfigured (default)
//!                                                               ├─ Simulator (opt-in)
//!                                                               └─ HTTP (not built)
//! ```

pub mod error;
pub mod fact;
pub mod policy;
pub mod simulator;
pub mod transport;
pub mod unconfigured;

pub use error::SyncError;
pub use fact::{
    FactEnvelope, FactOutcome, FactResult, InboundBatch, InboundKind, InboundMessage,
    OutboundBatch, OutboundReceipt, RefundAuthorizationDecision, RefundAuthorizationRequest,
};
pub use policy::{backoff_seconds, classify, form_batch, FactTransition, DEFAULT_BATCH_LIMIT};
pub use transport::SyncTransport;
