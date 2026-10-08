//! Shared runtime state: lifecycle status, revision counter, event
//! subscribers and in-flight accounting.

use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;
use std::time::Instant;

use edge_protocol::{CoreStatusV1, EdgeStateChangedV1, EventEnvelopeV1, EventV1};

const STARTING: u8 = 0;
const READY: u8 = 1;
const DEGRADED: u8 = 2;
const STOPPING: u8 = 3;

/// Lifecycle and fan-out state of the running daemon.
///
/// Status is an atomic rather than a lock so a poisoned mutex can never make
/// the daemon unable to report its own health.
#[derive(Debug)]
pub struct DaemonState {
    status: AtomicU8,
    revision: AtomicU64,
    in_flight: AtomicUsize,
    started: Instant,
    schema_version: i64,
    subscribers: Mutex<Vec<Sender<EventEnvelopeV1>>>,
}

impl DaemonState {
    pub fn new(schema_version: i64) -> Self {
        Self {
            status: AtomicU8::new(STARTING),
            revision: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
            started: Instant::now(),
            schema_version,
            subscribers: Mutex::new(Vec::new()),
        }
    }

    pub fn status(&self) -> CoreStatusV1 {
        match self.status.load(Ordering::SeqCst) {
            READY => CoreStatusV1::Ready,
            DEGRADED => CoreStatusV1::Degraded,
            STOPPING => CoreStatusV1::Stopping,
            _ => CoreStatusV1::Starting,
        }
    }

    pub fn set_status(&self, status: CoreStatusV1) {
        let encoded = match status {
            CoreStatusV1::Starting => STARTING,
            CoreStatusV1::Ready => READY,
            CoreStatusV1::Degraded => DEGRADED,
            CoreStatusV1::Stopping => STOPPING,
        };
        self.status.store(encoded, Ordering::SeqCst);
    }

    /// A storage failure degrades the runtime without taking it down: the
    /// register keeps answering queries and reporting its real health.
    pub fn mark_degraded(&self) {
        let _ = self
            .status
            .compare_exchange(READY, DEGRADED, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// A successful operation clears a previous degradation.
    pub fn mark_healthy(&self) {
        let _ = self
            .status
            .compare_exchange(DEGRADED, READY, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub fn schema_version(&self) -> i64 {
        self.schema_version
    }

    pub fn uptime_seconds(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }

    /// Monotonic within this daemon lifetime. Staleness signal only: never an
    /// aggregate version, never an input to a business rule.
    pub fn bump_revision(&self) -> u64 {
        self.revision.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    /// Count a request for the duration of its handling, so shutdown can drain.
    pub fn enter_request(&self) -> RequestGuard<'_> {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        RequestGuard { state: self }
    }

    /// Register an event subscriber. Dropping every sender (see
    /// [`Self::clear_subscribers`]) is what releases a parked event loop.
    pub fn subscribe(&self) -> Receiver<EventEnvelopeV1> {
        let (sender, receiver) = channel();
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.push(sender);
            tracing::debug!(subscribers = subscribers.len(), "event subscriber attached");
        }
        receiver
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers.lock().map(|s| s.len()).unwrap_or(0)
    }

    /// Fan out an event, dropping subscribers whose connection is gone.
    pub fn broadcast(&self, event: EventEnvelopeV1) {
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.retain(|sender| sender.send(event.clone()).is_ok());
        }
    }

    pub fn broadcast_state_changed(&self, reason: &str, revision: u64) {
        self.broadcast(EventEnvelopeV1::new(EventV1::EdgeStateChanged(
            EdgeStateChangedV1 {
                reason: reason.to_owned(),
                revision,
            },
        )));
    }

    pub fn clear_subscribers(&self) {
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.clear();
        }
    }
}

/// Keeps the in-flight counter accurate even if a handler returns early.
#[derive(Debug)]
pub struct RequestGuard<'a> {
    state: &'a DaemonState,
}

impl Drop for RequestGuard<'_> {
    fn drop(&mut self) {
        self.state.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_lifecycle_status() {
        let state = DaemonState::new(1);
        assert_eq!(state.status(), CoreStatusV1::Starting);
        state.set_status(CoreStatusV1::Ready);
        state.mark_degraded();
        assert_eq!(state.status(), CoreStatusV1::Degraded);
        state.mark_healthy();
        assert_eq!(state.status(), CoreStatusV1::Ready);
        // Degradation must not resurrect a stopping daemon.
        state.set_status(CoreStatusV1::Stopping);
        state.mark_degraded();
        assert_eq!(state.status(), CoreStatusV1::Stopping);
    }

    #[test]
    fn revision_is_monotonic() {
        let state = DaemonState::new(1);
        assert_eq!(state.revision(), 0);
        assert_eq!(state.bump_revision(), 1);
        assert_eq!(state.bump_revision(), 2);
        assert_eq!(state.revision(), 2);
    }

    #[test]
    fn broadcasts_to_live_subscribers_only() {
        let state = DaemonState::new(1);
        let first = state.subscribe();
        let second = state.subscribe();
        assert_eq!(state.subscriber_count(), 2);
        drop(second);

        state.broadcast_state_changed("ADD_ITEM", 7);
        let event = first.recv().unwrap();
        match event.event {
            EventV1::EdgeStateChanged(payload) => {
                assert_eq!(payload.reason, "ADD_ITEM");
                assert_eq!(payload.revision, 7);
            }
        }
        assert_eq!(state.subscriber_count(), 1);

        state.clear_subscribers();
        state.broadcast_state_changed("TOTALIZE", 8);
        assert!(first.recv().is_err());
    }

    #[test]
    fn in_flight_is_balanced() {
        let state = DaemonState::new(1);
        assert_eq!(state.in_flight(), 0);
        {
            let _guard = state.enter_request();
            assert_eq!(state.in_flight(), 1);
        }
        assert_eq!(state.in_flight(), 0);
    }
}
