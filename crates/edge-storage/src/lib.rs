//! SQLite-backed storage for PI.Edge.
//!
//! The database belongs to the core: no UI, no client and no external process
//! opens it. Durability settings are chosen for a POS terminal that can lose
//! power mid-sale:
//!
//! * `journal_mode = WAL` — readers never block the writer,
//! * `synchronous = FULL` — a committed sale survives an OS crash,
//! * `foreign_keys = ON` — referential integrity is enforced, not assumed,
//! * `busy_timeout` — contention waits instead of failing,
//! * forward-only migrations with checksums,
//! * one OS-level runtime lock per installation ([`lock`]), so exactly one
//!   process ever makes decisions about a register.

mod cash;
mod catalog;
pub mod error;
mod inventory;
pub mod lock;
pub mod migrations;
mod payments;
mod refunds;
pub mod sqlite;

pub use error::StorageError;
pub use lock::SingleWriterLock;
pub use sqlite::SqliteEdgeStore;
