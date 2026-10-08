//! The client abstraction.
//!
//! Every client of the PI.Edge core — `edge-cli`, the Tauri bridge, anything
//! we write later — depends on [`EdgeClient`] and on `edge-protocol` DTOs.
//! None of them can reach the core in-process: this crate does not depend on
//! `edge-application` or `edge-storage`, so "the UI owns the core" is not a
//! mistake that can be made by accident.
//!
//! ```text
//! CLI / Tauri bridge  ->  EdgeClient  ->  EdgeTransport  ->  edge-daemon
//! ```

pub mod client;
pub mod error;
pub mod events;
pub mod ipc;

pub use client::{ConnectionStatus, EdgeClient};
pub use error::ClientError;
pub use events::EventListener;
pub use ipc::IpcEdgeClient;

/// Version of the client library, reported in the handshake.
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
