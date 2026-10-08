//! PI.Edge application layer.
//!
//! Contains the use cases (commands) and queries that drive the POS. It
//! coordinates the domain with the ports declared in [`ports`] and never
//! references a concrete adapter (no SQLite, no HTTP, no UI).
//!
//! Every mutating use case runs inside a single store transaction, so the
//! aggregate, its journal event and its outbox event commit atomically.

pub mod adapters;
pub mod commands;
pub mod error;
pub mod ports;
pub mod queries;
pub mod service;

#[cfg(feature = "testing")]
pub mod testing;

pub use error::AppError;
pub use ports::{Clock, DeviceProfile, EdgeStore, IdFactory, UnitOfWork};
pub use service::EdgeApplication;

/// Version of the headless core, surfaced to clients for diagnostics.
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
