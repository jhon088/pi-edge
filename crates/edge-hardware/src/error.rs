//! Hardware failures.
//!
//! Five codes, deliberately. A client branches on *what it can do about it* —
//! retry, configure, give up — not on which device or vendor failed. The brand,
//! the model and the adapter's own words go in the message and the action log,
//! never in the contract (ADR 035).

use thiserror::Error;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum HardwareError {
    /// The device is configured but could not be reached. **Nothing was
    /// sent**, including when the connection attempt itself timed out: a
    /// socket that never opened carried no bytes (ADR 043).
    #[error("{device} is unavailable: {detail}")]
    Unavailable {
        device: &'static str,
        detail: String,
    },

    /// The device was reached, given the job, and did not answer in time.
    /// **Unresolved**: for a printer the paper may still come out, so this is
    /// never silently retried (ADR 037). Only produced after bytes went out —
    /// a connection that timed out is `Unavailable` (ADR 043).
    #[error("{device} timed out after {elapsed_ms}ms")]
    Timeout {
        device: &'static str,
        elapsed_ms: u64,
    },

    /// The transport failed **while** talking to the device, so part of the
    /// job may already be in it (ADR 043).
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

    /// What this failure says about whether the device got the job.
    ///
    /// The one question that decides whether a first receipt may be attempted
    /// again, and the adapter is the only thing that knows the answer — which
    /// is why the distinction lives in the taxonomy and not in a caller's
    /// guesswork (ADR 043).
    pub const fn certainty(&self) -> DeliveryCertainty {
        match self {
            // Nothing opened, nothing configured, nothing attempted.
            HardwareError::Unavailable { .. }
            | HardwareError::NotConfigured { .. }
            | HardwareError::ActionAlreadyCompleted { .. } => DeliveryCertainty::NotSent,
            // Bytes went out and the device never confirmed.
            HardwareError::Timeout { .. } | HardwareError::Io { .. } => {
                DeliveryCertainty::MayHaveBeenSent
            }
        }
    }
}

/// How much we know about whether a device acted.
///
/// Two values, because only two are actionable: either we can safely ask
/// again, or we cannot and a person has to look at the machine (ADR 043).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryCertainty {
    /// The device certainly did not get the job.
    NotSent,
    /// The device may have got it, in whole or in part. Unresolvable.
    MayHaveBeenSent,
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    /// The distinction the receipt lifecycle rests on.
    #[test]
    fn only_a_failure_after_the_bytes_went_out_is_uncertain() {
        for error in [
            HardwareError::Unavailable {
                device: "printer",
                detail: "nothing is listening".to_owned(),
            },
            HardwareError::Unavailable {
                device: "printer",
                detail: "the connection timed out after 2000ms".to_owned(),
            },
            HardwareError::NotConfigured {
                device: "printer",
                detail: "no driver".to_owned(),
            },
            HardwareError::ActionAlreadyCompleted {
                source_ref: "sale-1".to_owned(),
            },
        ] {
            assert_eq!(
                error.certainty(),
                DeliveryCertainty::NotSent,
                "{error} cannot have reached the device"
            );
        }

        for error in [
            HardwareError::Timeout {
                device: "printer",
                elapsed_ms: 2_000,
            },
            HardwareError::Io {
                device: "printer",
                detail: "the transport broke mid-write".to_owned(),
            },
        ] {
            assert_eq!(
                error.certainty(),
                DeliveryCertainty::MayHaveBeenSent,
                "{error} may have put paper out"
            );
        }
    }
}
