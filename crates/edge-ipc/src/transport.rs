//! The transport abstraction.
//!
//! Clients depend on [`EdgeTransport`] and never on a socket type, so the
//! platform can change without touching a single line of client or UI code.
//! The Edge Protocol stays identical across transports:
//!
//! * [`crate::DesktopLocalTransport`] — Unix domain socket on macOS/Linux,
//!   named pipe on Windows.
//! * `AndroidServiceTransport` — **future**. When PI.Edge runs on iMin, the
//!   Rust core will live inside an Android foreground service and the UI will
//!   reach it through Android IPC (Binder) or a unix socket inside the app
//!   sandbox; that decision is deferred until we can measure it on the device.
//!   Nothing outside this crate needs to change when it lands.
//!
//! The server side is symmetric: `edge-daemon` accepts connections through
//! [`EdgeListener`] and reads frames through [`ServerConnection`], so it never
//! names a socket either.

use std::fmt::Debug;

use edge_protocol::{
    ClientFrameV1, EventEnvelopeV1, RequestEnvelopeV1, ResponseEnvelopeV1, ServerFrameV1,
};

use crate::error::IpcError;

/// Client side of the Edge Protocol.
///
/// Implementations own the connection and authenticate it on `connect`. All
/// methods take `&self` so a UI can hold one transport in shared state.
pub trait EdgeTransport: Debug + Send + Sync {
    /// Open and authenticate the request channel if it is not open already.
    fn connect(&self) -> Result<(), IpcError>;

    /// Send one request and wait for the response with the same `request_id`.
    /// Connects on demand.
    fn request(&self, request: &RequestEnvelopeV1) -> Result<ResponseEnvelopeV1, IpcError>;

    /// Open a dedicated, independently authenticated event stream.
    fn subscribe(&self) -> Result<Box<dyn EdgeEventStream>, IpcError>;

    /// Drop the current channel and open a fresh authenticated one.
    fn reconnect(&self) -> Result<(), IpcError>;

    /// Close the request channel. Safe to call repeatedly.
    fn close(&self);

    fn is_connected(&self) -> bool;

    /// Diagnostic identity of the endpoint. Never a credential.
    fn endpoint_label(&self) -> String;
}

/// Server-to-client event stream. Lives on its own connection, so request
/// latency never competes with event delivery and neither side needs to
/// demultiplex frames.
pub trait EdgeEventStream: Debug + Send {
    /// Block until the next event. `Ok(None)` means the core closed the stream.
    fn next_event(&mut self) -> Result<Option<EventEnvelopeV1>, IpcError>;

    fn close(&mut self);
}

/// Server side: accepts client connections.
pub trait EdgeListener: Debug + Send + Sync {
    /// Accept the next connection.
    ///
    /// Implementations may block (the desktop one does, so that accepted
    /// streams stay blocking) or return `Ok(None)` when nothing is pending.
    /// Callers must therefore keep their shutdown handling off this call's
    /// thread.
    fn accept(&self) -> Result<Option<Box<dyn ServerConnection>>, IpcError>;

    fn endpoint_label(&self) -> String;

    /// Release the endpoint (remove the socket file, close the handle).
    fn shutdown(&self);
}

/// Server side: one client connection, framed.
pub trait ServerConnection: Debug + Send {
    /// Read the next client frame. `Ok(None)` means the client disconnected.
    fn receive(&mut self) -> Result<Option<ClientFrameV1>, IpcError>;

    fn send(&mut self, frame: &ServerFrameV1) -> Result<(), IpcError>;

    /// Diagnostic label for logs.
    fn peer_label(&self) -> String;
}

/// Convenience for implementations: send a response frame.
pub fn send_response(
    connection: &mut dyn ServerConnection,
    response: ResponseEnvelopeV1,
) -> Result<(), IpcError> {
    connection.send(&ServerFrameV1::Response(response))
}
