//! Hardware failures.
//!
//! Five codes, deliberately. A client branches on *what it can do about it* —
//! retry, configure, give up — not on which device or vendor failed. The brand,
//! the model and the adapter's own words go in the message and the action log,
//! never in the contract (ADR 035).

use thiserror::Error;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum HardwareError {
    /// The device is configured but could not be reached.
    #[error("{device} is unavailable: {detail}")]
    Unavailable {
        device: &'static str,
        detail: String,
    },

    /// The device was reached and did not answer in time. **Unresolved**: for a
    /// printer the paper may still come out, so this is never silently retried
    /// (ADR 037).
    #[error("{device} timed out after {elapsed_ms}ms")]
    Timeout {
        device: &'static str,
        elapsed_ms: u64,
    },

    /// The transport failed while talking to the device.
    #[error("{device} I/O failure: {detail}")]
    Io {
        device: &'static str,
        detail: String,
    },

    /// No driver configured for this device, or the configured driver has no
    /// bridge in this build. Never a silent fallback to another driver
    /// (ADR 038).
    #[error("{device} is not configured: {detail}")]
    NotConfigured {
        device: &'static str,
        detail: String,
    },

    /// The physical action this command would perform has already happened for
    /// the same cause. Returning this is the point: a drawer must not open
    /// twice because a command was replayed (ADR 037).
    #[error("this hardware action already completed for `{source_ref}`")]
    ActionAlreadyCompleted { source_ref: String },
}

impl HardwareError {
    /// Stable code. Part of the contract; the message never is.
    pub fn code(&self) -> &'static str {
        match self {
            HardwareError::Unavailable { .. } => "HARDWARE_UNAVAILABLE",
            HardwareError::Timeout { .. } => "HARDWARE_TIMEOUT",
            HardwareError::Io { .. } => "HARDWARE_IO_ERROR",
            HardwareError::NotConfigured { .. } => "HARDWARE_NOT_CONFIGURED",
            HardwareError::ActionAlreadyCompleted { .. } => "HARDWARE_ACTION_ALREADY_COMPLETED",
        }
    }

    /// Whether the physical effect may or may not have happened.
    ///
    /// A timeout is the honest "we do not know": the printer may have printed.
    /// Nothing automatic follows from it (ADR 037).
    pub const fn is_unresolved(&self) -> bool {
        matches!(self, HardwareError::Timeout { .. })
    }
}
