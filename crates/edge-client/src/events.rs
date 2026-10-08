//! Background event listener.
//!
//! Owns a thread that keeps an event stream open and hands every
//! `EdgeStateChanged` to a callback. When the core goes away the thread waits
//! and re-subscribes, so a UI gets push updates again as soon as the daemon is
//! back without polling in between.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use edge_protocol::EventEnvelopeV1;

use crate::ipc::IpcEdgeClient;

/// Delay between re-subscription attempts while the core is down.
const RESUBSCRIBE_DELAY: Duration = Duration::from_millis(500);

/// A running event listener. Dropping it asks the thread to stop.
#[derive(Debug)]
pub struct EventListener {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl EventListener {
    /// Start listening. `on_event` runs on the listener thread, so it should
    /// hand off quickly (emit to the UI, wake a refresh) rather than block.
    pub fn spawn<F>(client: Arc<IpcEdgeClient>, on_event: F) -> Self
    where
        F: Fn(EventEnvelopeV1) + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("edge-events".to_owned())
            .spawn(move || listen(client, &thread_stop, on_event))
            .ok();
        Self { stop, handle }
    }

    /// Signal the thread to stop after its current blocking read.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.handle
            .as_ref()
            .map(|handle| !handle.is_finished())
            .unwrap_or(false)
    }
}

impl Drop for EventListener {
    fn drop(&mut self) {
        self.stop();
        // Not joined on purpose: the thread may be parked in a blocking read
        // on the event socket, and a UI shutdown must not wait for the next
        // event to arrive. The thread ends when the stream closes or the
        // process exits.
    }
}

fn listen<F>(client: Arc<IpcEdgeClient>, stop: &Arc<AtomicBool>, on_event: F)
where
    F: Fn(EventEnvelopeV1) + Send + 'static,
{
    while !stop.load(Ordering::SeqCst) {
        match client.subscribe() {
            Ok(mut stream) => {
                tracing::debug!("event stream established");
                loop {
                    if stop.load(Ordering::SeqCst) {
                        stream.close();
                        return;
                    }
                    match stream.next_event() {
                        Ok(Some(event)) => on_event(event),
                        Ok(None) => {
                            tracing::debug!("event stream closed by the core");
                            break;
                        }
                        Err(error) => {
                            tracing::debug!(%error, "event stream failed");
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                tracing::trace!(%error, "could not subscribe; will retry");
            }
        }
        std::thread::sleep(RESUBSCRIBE_DELAY);
    }
}
