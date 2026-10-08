//! Desktop transport: Unix domain socket on macOS/Linux, named pipe on
//! Windows.
//!
//! This module is the only place in the workspace that mentions either
//! primitive. `interprocess` gives one API over both; the choice of primitive
//! depends solely on the platform and the endpoint kind.

use std::fmt;
use std::io::BufReader;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use edge_protocol::error_codes;
use edge_protocol::{
    ClientFrameV1, ErrorV1, EventEnvelopeV1, HelloV1, OperationV1, RequestEnvelopeV1,
    ResponseEnvelopeV1, ServerFrameV1, PROTOCOL_VERSION,
};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ListenerNonblockingMode, ListenerOptions, Name, RecvHalf,
    SendHalf,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use uuid::Uuid;

use crate::codec;
use crate::credentials::{FileCredentialProvider, LocalCredentialProvider};
use crate::error::IpcError;
use crate::paths::{Endpoint, EndpointInner, RuntimePaths};
use crate::transport::{EdgeEventStream, EdgeListener, EdgeTransport, ServerConnection};

fn endpoint_name(endpoint: &Endpoint) -> Result<Name<'_>, IpcError> {
    match endpoint.inner() {
        EndpointInner::Filesystem(path) => Ok(path.as_os_str().to_fs_name::<GenericFilePath>()?),
        EndpointInner::Namespaced(name) => Ok(name.as_str().to_ns_name::<GenericNamespaced>()?),
    }
}

/// A framed, bidirectional connection. Holds the only socket types in the
/// workspace.
struct FrameChannel {
    reader: BufReader<RecvHalf>,
    writer: SendHalf,
}

impl fmt::Debug for FrameChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FrameChannel")
    }
}

impl FrameChannel {
    fn connect(endpoint: &Endpoint) -> Result<Self, IpcError> {
        let name = endpoint_name(endpoint)?;
        let stream = LocalSocketStream::connect(name).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::AddrNotAvailable => IpcError::NotConnected {
                endpoint: endpoint.label().to_owned(),
            },
            _ => IpcError::Io(error),
        })?;
        Ok(Self::from_stream(stream))
    }

    fn from_stream(stream: LocalSocketStream) -> Self {
        let (reader, writer) = stream.split();
        Self {
            reader: BufReader::new(reader),
            writer,
        }
    }

    fn send<T: Serialize>(&mut self, frame: &T) -> Result<(), IpcError> {
        codec::write_frame(&mut self.writer, frame)
    }

    fn receive<T: DeserializeOwned>(&mut self) -> Result<Option<T>, IpcError> {
        codec::read_frame(&mut self.reader)
    }
}

/// Request/response exchange on an already authenticated channel.
fn exchange(
    channel: &mut FrameChannel,
    request: &RequestEnvelopeV1,
) -> Result<ResponseEnvelopeV1, IpcError> {
    channel.send(&ClientFrameV1::Request(request.clone()))?;
    loop {
        match channel.receive::<ServerFrameV1>()? {
            Some(ServerFrameV1::Response(response))
                if response.request_id == request.request_id =>
            {
                return Ok(response)
            }
            // Ignore anything that is not ours: a late response to an abandoned
            // request, or an event on a request channel.
            Some(_) => continue,
            None => return Err(IpcError::ConnectionLost),
        }
    }
}

/// Client transport for desktop platforms.
pub struct DesktopLocalTransport {
    endpoint: Endpoint,
    credentials: Arc<dyn LocalCredentialProvider>,
    client_name: String,
    client_version: String,
    channel: Mutex<Option<FrameChannel>>,
}

impl fmt::Debug for DesktopLocalTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DesktopLocalTransport")
            .field("endpoint", &self.endpoint.label())
            .field("client_name", &self.client_name)
            .field("connected", &self.is_connected())
            .finish()
    }
}

impl DesktopLocalTransport {
    pub fn new(
        endpoint: Endpoint,
        credentials: Arc<dyn LocalCredentialProvider>,
        client_name: impl Into<String>,
        client_version: impl Into<String>,
    ) -> Self {
        Self {
            endpoint,
            credentials,
            client_name: client_name.into(),
            client_version: client_version.into(),
            channel: Mutex::new(None),
        }
    }

    /// Resolve endpoint and credential from the database path, the way every
    /// client does it. No credential is ever passed on a command line.
    pub fn for_database(
        database: impl AsRef<Path>,
        client_name: impl Into<String>,
        client_version: impl Into<String>,
    ) -> Result<Self, IpcError> {
        let paths = RuntimePaths::for_database(database)?;
        let credentials = Arc::new(FileCredentialProvider::new(paths.token_file()));
        Ok(Self::new(
            paths.endpoint()?,
            credentials,
            client_name,
            client_version,
        ))
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    fn lock(&self) -> Result<MutexGuard<'_, Option<FrameChannel>>, IpcError> {
        self.channel
            .lock()
            .map_err(|_| IpcError::Codec("transport mutex poisoned".to_owned()))
    }

    /// Connect and complete the `HELLO` handshake. Every connection
    /// authenticates on its own.
    fn open_authenticated(&self) -> Result<FrameChannel, IpcError> {
        let mut channel = FrameChannel::connect(&self.endpoint)?;
        let token = self.credentials.load()?;
        channel.send(&ClientFrameV1::Hello(HelloV1 {
            protocol_version: PROTOCOL_VERSION,
            client_name: self.client_name.clone(),
            client_version: self.client_version.clone(),
            auth_token: token.expose().to_owned(),
        }))?;
        match channel.receive::<ServerFrameV1>()? {
            Some(ServerFrameV1::Welcome(welcome)) => {
                if welcome.protocol_version != PROTOCOL_VERSION {
                    return Err(IpcError::ProtocolVersionMismatch {
                        client: PROTOCOL_VERSION,
                        core: welcome.protocol_version,
                    });
                }
                tracing::debug!(
                    endpoint = %self.endpoint.label(),
                    core_version = %welcome.core_version,
                    "authenticated against the edge core"
                );
                Ok(channel)
            }
            Some(ServerFrameV1::Response(response)) => {
                let error = response.error.unwrap_or_else(|| {
                    ErrorV1::new(error_codes::AUTHENTICATION_FAILED, "connection rejected")
                });
                Err(IpcError::Rejected {
                    code: error.code,
                    message: error.message,
                })
            }
            Some(_) => Err(IpcError::UnexpectedFrame {
                expected: "WELCOME",
            }),
            None => Err(IpcError::ConnectionLost),
        }
    }
}

impl EdgeTransport for DesktopLocalTransport {
    fn connect(&self) -> Result<(), IpcError> {
        let mut guard = self.lock()?;
        if guard.is_none() {
            *guard = Some(self.open_authenticated()?);
        }
        Ok(())
    }

    fn request(&self, request: &RequestEnvelopeV1) -> Result<ResponseEnvelopeV1, IpcError> {
        let mut guard = self.lock()?;
        if guard.is_none() {
            *guard = Some(self.open_authenticated()?);
        }
        let outcome = match guard.as_mut() {
            Some(channel) => exchange(channel, request),
            None => Err(IpcError::ConnectionLost),
        };
        if outcome.is_err() {
            // A broken channel is never reused; the next call reconnects.
            *guard = None;
        }
        outcome
    }

    fn subscribe(&self) -> Result<Box<dyn EdgeEventStream>, IpcError> {
        let mut channel = self.open_authenticated()?;
        let request_id = Uuid::new_v4();
        channel.send(&ClientFrameV1::Request(RequestEnvelopeV1::new(
            request_id,
            OperationV1::Subscribe,
        )))?;
        loop {
            match channel.receive::<ServerFrameV1>()? {
                Some(ServerFrameV1::Response(response)) if response.request_id == request_id => {
                    if !response.success {
                        let error = response.error.unwrap_or_else(|| {
                            ErrorV1::new(error_codes::UNSUPPORTED_OPERATION, "subscribe refused")
                        });
                        return Err(IpcError::Rejected {
                            code: error.code,
                            message: error.message,
                        });
                    }
                    break;
                }
                Some(_) => continue,
                None => return Err(IpcError::ConnectionLost),
            }
        }
        Ok(Box::new(DesktopEventStream {
            channel: Some(channel),
        }))
    }

    fn reconnect(&self) -> Result<(), IpcError> {
        let mut guard = self.lock()?;
        *guard = None;
        *guard = Some(self.open_authenticated()?);
        Ok(())
    }

    fn close(&self) {
        if let Ok(mut guard) = self.lock() {
            *guard = None;
        }
    }

    fn is_connected(&self) -> bool {
        self.channel
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }

    fn endpoint_label(&self) -> String {
        self.endpoint.label().to_owned()
    }
}

struct DesktopEventStream {
    channel: Option<FrameChannel>,
}

impl fmt::Debug for DesktopEventStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DesktopEventStream")
            .field("open", &self.channel.is_some())
            .finish()
    }
}

impl EdgeEventStream for DesktopEventStream {
    fn next_event(&mut self) -> Result<Option<EventEnvelopeV1>, IpcError> {
        loop {
            let Some(channel) = self.channel.as_mut() else {
                return Ok(None);
            };
            match channel.receive::<ServerFrameV1>() {
                Ok(Some(ServerFrameV1::Event(event))) => return Ok(Some(event)),
                Ok(Some(_)) => continue,
                Ok(None) => {
                    self.channel = None;
                    return Ok(None);
                }
                Err(error) => {
                    self.channel = None;
                    return Err(error);
                }
            }
        }
    }

    fn close(&mut self) {
        self.channel = None;
    }
}

/// Server-side listener for desktop platforms.
pub struct DesktopLocalListener {
    endpoint: Endpoint,
    listener: LocalSocketListener,
}

impl fmt::Debug for DesktopLocalListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DesktopLocalListener")
            .field("endpoint", &self.endpoint.label())
            .finish()
    }
}

impl DesktopLocalListener {
    /// Bind the endpoint.
    ///
    /// The caller must already hold the single-writer lock, which is what makes
    /// removing a leftover socket file safe: if we own the lock, any existing
    /// endpoint belongs to a dead process.
    pub fn bind(endpoint: Endpoint) -> Result<Self, IpcError> {
        if let Some(path) = endpoint.filesystem_path() {
            if path.exists() {
                std::fs::remove_file(path)?;
            }
        }
        let listener = {
            let name = endpoint_name(&endpoint)?;
            // Blocking on purpose. A non-blocking listener makes the *accepted*
            // stream non-blocking too on BSD and macOS (the flag is inherited),
            // which turns every framed read into a spurious `EAGAIN`. The
            // daemon keeps its accept loop responsive with a dedicated thread
            // instead.
            ListenerOptions::new()
                .name(name)
                .nonblocking(ListenerNonblockingMode::Neither)
                .create_sync()?
        };
        #[cfg(unix)]
        if let Some(path) = endpoint.filesystem_path() {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self { endpoint, listener })
    }
}

impl EdgeListener for DesktopLocalListener {
    fn accept(&self) -> Result<Option<Box<dyn ServerConnection>>, IpcError> {
        match self.listener.accept() {
            Ok(stream) => Ok(Some(Box::new(DesktopServerConnection {
                channel: FrameChannel::from_stream(stream),
            }))),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(IpcError::Io(error)),
        }
    }

    fn endpoint_label(&self) -> String {
        self.endpoint.label().to_owned()
    }

    fn shutdown(&self) {
        if let Some(path) = self.endpoint.filesystem_path() {
            if let Err(error) = std::fs::remove_file(path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        endpoint = %self.endpoint.label(),
                        %error,
                        "could not remove the endpoint file"
                    );
                }
            }
        }
    }
}

struct DesktopServerConnection {
    channel: FrameChannel,
}

impl fmt::Debug for DesktopServerConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DesktopServerConnection")
    }
}

impl ServerConnection for DesktopServerConnection {
    fn receive(&mut self) -> Result<Option<ClientFrameV1>, IpcError> {
        self.channel.receive()
    }

    fn send(&mut self, frame: &ServerFrameV1) -> Result<(), IpcError> {
        self.channel.send(frame)
    }

    fn peer_label(&self) -> String {
        "local".to_owned()
    }
}
