//! Sync failures.
//!
//! Four, and the distinction that matters is **whether the remote may already
//! have the facts**. A timeout is the honest "we asked and we do not know", and
//! the only safe answer is to send the same `event_id` again and let the
//! consumer deduplicate — at-least-once, on purpose (ADR 004).

use thiserror::Error;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum SyncError {
    /// No transport configured for this installation. PI.Edge keeps selling;
    /// facts keep accumulating durably (ADR 040).
    #[error("no sync transport is configured for this installation")]
    NotConfigured,

    /// The remote could not be reached.
    #[error("the sync transport is unavailable: {detail}")]
    Unavailable { detail: String },

    /// Sent and not answered in time. **Unresolved**: the remote may have the
    /// batch, so nothing is ever concluded from this.
    #[error("the sync transport timed out after {elapsed_ms}ms")]
    Timeout { elapsed_ms: u64 },

    /// The remote answered something this contract cannot read — an unknown
    /// message kind, a missing field, a result for a fact nobody sent. The
    /// whole exchange is discarded rather than half-applied (ADR 041).
    #[error("the sync transport answered something unreadable: {detail}")]
    Malformed { detail: String },
}

impl SyncError {
    /// Stable code. Part of the contract; the message never is.
    pub fn code(&self) -> &'static str {
        match self {
            SyncError::NotConfigured => "SYNC_NOT_CONFIGURED",
            SyncError::Unavailable { .. } => "SYNC_UNAVAILABLE",
            SyncError::Timeout { .. } => "SYNC_TIMEOUT",
            SyncError::Malformed { .. } => "SYNC_MALFORMED_RESPONSE",
        }
    }

    /// Whether the remote may already hold what we sent.
    ///
    /// Only a timeout is ambiguous. `Unavailable` means the batch never left;
    /// `Malformed` means it arrived and the *answer* was unreadable, which is
    /// equally ambiguous — so both of those that could have landed are treated
    /// as unresolved.
    pub const fn may_have_landed(&self) -> bool {
        matches!(
            self,
            SyncError::Timeout { .. } | SyncError::Malformed { .. }
        )
    }
}
