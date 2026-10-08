//! Local IPC for PI.Edge.
//!
//! This is the **only** crate that knows what a Unix domain socket or a Windows
//! named pipe is. Everything above it speaks [`EdgeTransport`] and the
//! `edge-protocol` frames; the domain never sees JSON, sockets or pipes.
//!
//! ```text
//! EdgeClient  ->  EdgeTransport  ->  DesktopLocalTransport   (UDS / named pipe)
//!                               ->  AndroidServiceTransport (future: Binder or UDS)
//! edge-daemon ->  EdgeListener   ->  DesktopLocalListener
//! ```
//!
//! Wire format is JSON Lines (see [`codec`]). Authentication is a single
//! `HELLO` frame per connection carrying a local credential (see
//! [`credentials`]).

pub mod codec;
pub mod credentials;
pub mod error;
pub mod paths;
pub mod transport;

mod desktop;

pub use desktop::{DesktopLocalListener, DesktopLocalTransport};
pub use error::IpcError;
pub use paths::{Endpoint, RuntimePaths};
pub use transport::{EdgeEventStream, EdgeListener, EdgeTransport, ServerConnection};
