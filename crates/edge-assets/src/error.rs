//! Asset failures.
//!
//! Stable codes, like every other context: a client branches on *what it can
//! do about it* — send the bytes, send them again, fix the declaration, free
//! some disk — never on a path or an OS message. Paths never appear in a
//! variant, because a path is not part of any contract (ADR 045).

use thiserror::Error;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum AssetError {
    /// This installation has no asset store. Never a silent fallback to a
    /// remote URL, and never a temporary directory nobody asked for: a
    /// snapshot that needs assets simply cannot be activated here (ADR 045).
    #[error("this installation has no local asset store")]
    NotConfigured,

    /// The declaration itself is unusable, so no bytes are accepted at all.
    #[error("asset `{content_hash}` is not acceptable: {reason}")]
    Rejected {
        content_hash: String,
        reason: String,
    },

    /// The declared size is above this installation's limit.
    #[error("asset `{content_hash}` declares {declared} bytes, above the limit of {limit}")]
    TooLarge {
        content_hash: String,
        declared: i64,
        limit: i64,
    },

    /// The bytes do not hash to what was declared. **The bytes are wrong, not
    /// the hash**: the hash is the identity, so the staged file is discarded
    /// and the import can start again cleanly.
    #[error("asset `{declared}` hashes to `{computed}`")]
    DigestMismatch { declared: String, computed: String },

    /// What is on disk is not the declared length. A staged file that is still
    /// short is resumable; one that is already too long is wrong and is
    /// discarded.
    #[error("asset `{content_hash}` is {actual} bytes, not the declared {declared}")]
    SizeMismatch {
        content_hash: String,
        declared: i64,
        actual: i64,
    },

    /// Commit arrived with nothing staged for this asset.
    #[error("no bytes have been staged for asset `{content_hash}`")]
    NotStaged { content_hash: String },

    /// A chunk that would leave a gap, or overshoot what is staged. Accepting
    /// it would produce a file that hashes to nothing.
    #[error("chunk for `{content_hash}` starts at {offset}; {expected} bytes are staged")]
    ChunkOutOfOrder {
        content_hash: String,
        expected: i64,
        offset: i64,
    },

    /// Asked to read an asset this installation does not have, or has only
    /// partially, or has in a state that does not match its declaration.
    #[error("asset `{content_hash}` is not available locally ({state})")]
    NotReady {
        content_hash: String,
        state: &'static str,
    },

    /// The disk is full. Explicit on purpose: nothing is ever deleted to make
    /// room, so the operator decides (ADR 045).
    #[error("no space left for asset `{content_hash}`")]
    OutOfSpace { content_hash: String },

    /// Anything else the filesystem said, with its own words kept for the log
    /// and the operator — never a path.
    #[error("asset store failed to {operation}: {reason}")]
    Io {
        operation: &'static str,
        reason: String,
    },
}

impl AssetError {
    /// Stable code. Part of the contract; the message never is.
    pub fn code(&self) -> &'static str {
        match self {
            AssetError::NotConfigured => "ASSET_STORE_NOT_CONFIGURED",
            AssetError::Rejected { .. } => "ASSET_REJECTED",
            AssetError::TooLarge { .. } => "ASSET_TOO_LARGE",
            AssetError::DigestMismatch { .. } => "ASSET_DIGEST_MISMATCH",
            AssetError::SizeMismatch { .. } => "ASSET_SIZE_MISMATCH",
            AssetError::NotStaged { .. } => "ASSET_NOT_STAGED",
            AssetError::ChunkOutOfOrder { .. } => "ASSET_CHUNK_OUT_OF_ORDER",
            AssetError::NotReady { .. } => "ASSET_NOT_READY",
            AssetError::OutOfSpace { .. } => "ASSET_STORE_FULL",
            AssetError::Io { .. } => "ASSET_IO_FAILURE",
        }
    }
}
