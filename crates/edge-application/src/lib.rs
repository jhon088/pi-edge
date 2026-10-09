//! PI.Edge application layer.
//!
//! Contains the use cases (commands) and queries that drive the POS. It
//! coordinates the domain with the ports declared in [`ports`] and never
//! references a concrete adapter (no SQLite, no HTTP, no UI).
//!
//! Every mutating use case runs inside a single store transaction, so the
//! aggregate, its journal event and its outbox event commit atomically.

pub mod cash_service;
pub mod catalog_service;
pub mod commands;
/// Shared plumbing every use case needs: replay protection, journal
/// construction and the single-terminal context rule.
pub(crate) mod context;
pub mod error;
pub mod inventory_service;
pub mod payment_service;
pub mod ports;
pub mod queries;
pub mod refund_service;
pub mod service;

#[cfg(feature = "testing")]
pub mod testing;

pub use error::AppError;
pub use payment_service::PaymentOutcome;
pub use ports::{
    CashUnitOfWork, Clock, DeviceProfile, EdgeStore, IdFactory, InventoryUnitOfWork,
    PaymentUnitOfWork, SystemClock, UnitOfWork, UuidFactory,
};
pub use service::EdgeApplication;

/// Version of the headless core, surfaced to clients for diagnostics.
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
