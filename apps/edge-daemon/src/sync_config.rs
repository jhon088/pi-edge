//! Whether this installation talks to a server, and through what.
//!
//! Configuration lives **here**, in the composition root, like the hardware
//! next door and for the same reason: nothing downstream may name a host, a
//! URL or a credential (ADR 039, ADR 040).
//!
//! ```text
//! PI_EDGE_SYNC=simulator | none
//! ```
//!
//! **A simulator is opt-in.** An absent variable, or `none`, means *no sync
//! transport*: every sync operation answers `SYNC_NOT_CONFIGURED` and PI.Edge
//! keeps selling, keeps writing durable facts, and keeps them. That is not a
//! degraded mode — it is how an installation without a server runs,
//! indefinitely.
//!
//! There is no HTTP driver to configure because there is no server to talk to
//! yet. When there is, it arrives as one more arm of `transport_of` and
//! nothing above this file changes.

use std::sync::Arc;

use edge_sync::simulator::{SimulatorSyncTransport, SIMULATOR};
use edge_sync::SyncTransport;

pub const SYNC_ENV: &str = "PI_EDGE_SYNC";

/// The value, and the meaning of an absent variable: no transport.
const NONE: &str = "none";

/// Build the sync transport this runtime uses, if any.
///
/// `None` is the normal answer. An unrecognised value refuses to start rather
/// than falling back: a till that syncs somewhere nobody asked for is worse
/// than one that will not boot.
pub fn from_env() -> Result<Option<Arc<dyn SyncTransport>>, String> {
    let spec = std::env::var(SYNC_ENV).unwrap_or_else(|_| NONE.to_owned());
    transport_of(spec.trim())
}

fn transport_of(spec: &str) -> Result<Option<Arc<dyn SyncTransport>>, String> {
    match spec {
        "" | NONE => Ok(None),
        SIMULATOR => {
            tracing::warn!(
                "sync is the deterministic simulator; no PI.COMMERCE Server is contacted. \
                 Not for production use"
            );
            Ok(Some(Arc::new(SimulatorSyncTransport::new())))
        }
        other => Err(format!(
            "`{other}` is not a known {SYNC_ENV} value (expected `{SIMULATOR}` or `{NONE}`)"
        )),
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use edge_sync::unconfigured::UNCONFIGURED;

    #[test]
    fn nothing_configured_means_no_transport() {
        // The blocker from milestone 10, applied to sync: absence selects
        // nothing, and certainly not a simulator.
        for spec in ["", "none", "   "] {
            assert!(
                transport_of(spec.trim()).expect("a known spec").is_none(),
                "`{spec}` must not select a transport"
            );
        }
    }

    #[test]
    fn the_simulator_is_only_selected_when_it_is_named() {
        let transport = transport_of(SIMULATOR)
            .expect("a known spec")
            .expect("the simulator was asked for");
        assert_eq!(transport.driver(), SIMULATOR);
        assert_ne!(transport.driver(), UNCONFIGURED);
    }

    #[test]
    fn an_unknown_driver_refuses_to_start() {
        let error = transport_of("https://pi.commerce.example")
            .expect_err("an unknown driver is a configuration error");
        assert!(error.contains(SYNC_ENV), "{error}");
    }
}
