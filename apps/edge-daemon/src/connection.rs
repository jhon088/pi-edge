//! One client connection: handshake, then requests or events.
//!
//! A connection is either a request channel or an event stream, never both.
//! That keeps exactly one thread writing to a socket, so no frame can ever be
//! interleaved with another.

use std::sync::Arc;

use edge_application::CORE_VERSION;
use edge_ipc::credentials::AuthToken;
use edge_ipc::{IpcError, ServerConnection};
use edge_protocol::{
    error_codes, ClientFrameV1, ErrorV1, HelloV1, OperationV1, ResponseEnvelopeV1, ServerFrameV1,
    WelcomeV1, PROTOCOL_VERSION,
};
use uuid::Uuid;

use crate::dispatch::Dispatcher;

/// Everything a connection handler needs.
#[derive(Debug)]
pub struct ConnectionContext {
    dispatcher: Arc<Dispatcher>,
    token: AuthToken,
}

impl ConnectionContext {
    pub fn new(dispatcher: Arc<Dispatcher>, token: AuthToken) -> Self {
        Self { dispatcher, token }
    }
}

/// Serve one connection until the peer disconnects.
pub fn serve(mut connection: Box<dyn ServerConnection>, context: Arc<ConnectionContext>) {
    let peer = connection.peer_label();
    match handshake(connection.as_mut(), &context) {
        Ok(hello) => {
            tracing::info!(
                peer = %peer,
                client = %hello.client_name,
                client_version = %hello.client_version,
                "client authenticated"
            );
        }
        Err(reason) => {
            tracing::warn!(peer = %peer, reason, "connection rejected");
            return;
        }
    }

    loop {
        match connection.receive() {
            Ok(Some(ClientFrameV1::Request(request))) => {
                let request_id = request.request_id;
                if matches!(request.operation, OperationV1::Subscribe) {
                    // Register before acknowledging: once the client sees the
                    // ack it must be impossible to miss an event.
                    let events = context.dispatcher.state().subscribe();
                    let response =
                        ResponseEnvelopeV1::ok(request_id, edge_protocol::ResultV1::Acknowledged);
                    if send(connection.as_mut(), &ServerFrameV1::Response(response)).is_err() {
                        return;
                    }
                    pump_events(connection, events);
                    return;
                }
                let response = context.dispatcher.handle(request);
                if send(connection.as_mut(), &ServerFrameV1::Response(response)).is_err() {
                    return;
                }
            }
            Ok(Some(ClientFrameV1::Hello(_))) => {
                // Authentication is once per connection; a second HELLO is a
                // client bug, not a re-auth opportunity.
                let response = ResponseEnvelopeV1::failed(
                    Uuid::nil(),
                    ErrorV1::new(
                        error_codes::UNSUPPORTED_OPERATION,
                        "this connection is already authenticated",
                    ),
                );
                let _ = send(connection.as_mut(), &ServerFrameV1::Response(response));
                return;
            }
            Ok(None) => {
                tracing::debug!(peer = %peer, "client disconnected");
                return;
            }
            Err(error) => {
                // The stream may be desynchronized, so answer once and close.
                tracing::warn!(peer = %peer, %error, "malformed frame");
                let response = ResponseEnvelopeV1::failed(
                    Uuid::nil(),
                    ErrorV1::new(error.code(), error.to_string()),
                );
                let _ = send(connection.as_mut(), &ServerFrameV1::Response(response));
                return;
            }
        }
    }
}

/// Expect exactly one `HELLO` frame and validate it.
///
/// Failures are deliberately indistinguishable: the client learns that the
/// connection was rejected, never whether a credential exists, how long it
/// should be, or where it lives.
fn handshake(
    connection: &mut dyn ServerConnection,
    context: &ConnectionContext,
) -> Result<HelloV1, &'static str> {
    let frame = match connection.receive() {
        Ok(Some(frame)) => frame,
        Ok(None) => return Err("closed before handshake"),
        Err(_) => {
            reject(
                connection,
                error_codes::MALFORMED_FRAME,
                "malformed handshake",
            );
            return Err("malformed handshake");
        }
    };

    let hello = match frame {
        ClientFrameV1::Hello(hello) => hello,
        ClientFrameV1::Request(_) => {
            reject(
                connection,
                error_codes::NOT_AUTHENTICATED,
                "authenticate before sending requests",
            );
            return Err("request before handshake");
        }
    };

    if hello.protocol_version != PROTOCOL_VERSION {
        reject(
            connection,
            error_codes::PROTOCOL_VERSION_MISMATCH,
            "unsupported protocol version",
        );
        return Err("protocol version mismatch");
    }

    if !context.token.verify(&hello.auth_token) {
        reject(
            connection,
            error_codes::AUTHENTICATION_FAILED,
            "connection rejected",
        );
        return Err("authentication failed");
    }

    let welcome = ServerFrameV1::Welcome(WelcomeV1 {
        protocol_version: PROTOCOL_VERSION,
        core_version: CORE_VERSION.to_owned(),
        status: context.dispatcher.state().status(),
        revision: context.dispatcher.state().revision(),
    });
    if send(connection, &welcome).is_err() {
        return Err("could not send welcome");
    }
    Ok(hello)
}

fn reject(connection: &mut dyn ServerConnection, code: &str, message: &str) {
    let response = ResponseEnvelopeV1::failed(Uuid::nil(), ErrorV1::new(code, message));
    let _ = send(connection, &ServerFrameV1::Response(response));
}

fn send(connection: &mut dyn ServerConnection, frame: &ServerFrameV1) -> Result<(), IpcError> {
    connection.send(frame)
}

/// Turn the connection into a one-way event stream.
///
/// Parks on the subscriber channel; shutdown drops every sender, which wakes
/// this loop and ends the thread.
fn pump_events(
    mut connection: Box<dyn ServerConnection>,
    events: std::sync::mpsc::Receiver<edge_protocol::EventEnvelopeV1>,
) {
    let peer = connection.peer_label();
    tracing::info!(peer = %peer, "connection promoted to event stream");
    while let Ok(event) = events.recv() {
        if connection.send(&ServerFrameV1::Event(event)).is_err() {
            break;
        }
    }
    tracing::debug!(peer = %peer, "event stream closed");
}
