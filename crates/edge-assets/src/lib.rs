//! Local asset runtime for PI.Edge.
//!
//! A `CatalogSnapshot` references images by content hash (milestone 13A); this
//! crate owns the bytes behind those references, so a POS or a kiosk shows a
//! product photo with **no network at all** (ADR 045).
//!
//! ```text
//! AssetRef { content_hash, media_type, byte_size }   the catalog's promise
//!     │
//!     ├─ write_chunk … → staging          resumable, never served
//!     ├─ commit          → verified       size + SHA-256 + media type
//!     └─ open            → bytes          offline, streamed, no path exposed
//! ```
//!
//! Three things this crate deliberately does not do:
//!
//! * **No HTTP, and no downloader.** It is handed bytes. Whether they came
//!   from a sync, a bundle, a USB stick or a test is not its business, which
//!   is what keeps the store usable before any transport exists.
//! * **No database.** The content store is the state: a file at the path its
//!   own digest dictates. Nothing to keep in step, and nothing to recover.
//! * **No image processing.** No resize, no transcode, no thumbnails. The
//!   stored bytes hash to exactly what the catalog declared, because they are
//!   exactly what it declared.

pub mod error;
pub mod store;

pub use error::AssetError;
pub use store::{
    AssetState, FileSystemAssetStore, LocalAssetStore, UnconfiguredAssetStore,
    DEFAULT_MAX_ASSET_BYTES,
};

/// Re-exported so a caller can name what it is storing without depending on
/// the catalog for one type.
pub use edge_catalog::AssetRef;
