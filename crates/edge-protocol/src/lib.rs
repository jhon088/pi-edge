//! Versioned wire contract for PI.Edge clients.
//!
//! Every client — `edge-cli`, the Tauri diagnostic UI, a future Android host —
//! talks to `edge-daemon` through the types in this crate. Rules:
//!
//! * **No internal struct is ever serialized to the wire.** Domain aggregates
//!   and application commands stay inside the core; this crate holds explicit,
//!   versioned DTOs.
//! * Money is carried as minor units **and** a core-rendered string, so no
//!   client formats or recomputes an official amount.
//! * Derived values (paid total, balance due, change due) are computed by the
//!   core.
//! * DTOs are suffixed with their version. Breaking changes add `V2`; `V1` is
//!   never mutated.
//! * This crate knows nothing about sockets, pipes or framing. Transport lives
//!   in `edge-ipc`.
//!
//! With default features this crate depends only on `serde`/`uuid`, so a UI
//! client does not link the application layer. The `core-mapping` feature adds
//! the conversions used by the daemon and the in-process client.

pub mod dto;
pub mod envelope;
pub mod error_codes;

#[cfg(feature = "catalog-mapping")]
pub mod content_mapping;
#[cfg(feature = "core-mapping")]
pub mod mapping;

pub use dto::*;
pub use envelope::*;

/// Wire contract version advertised in every envelope.
pub const PROTOCOL_VERSION: u32 = 1;
