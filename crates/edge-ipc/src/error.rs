//! Transport-level errors.
//!
//! Nothing here ever carries a credential, and `NotConnected`/`Rejected` never
//! disclose why a handshake failed.

use edge_protocol::error_codes;

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("the edge core is not reachable at {endpoint}")]
    NotConnected { endpoint: String },

    #[error("the connection to the edge core was lost")]
    ConnectionLost,

    #[error("io failure on the local transport: {0}")]
    Io(#[from] std::io::Error),

    #[error("frame could not be encoded or decoded: {0}")]
    Codec(String),

    #[error("frame exceeds the {limit} byte limit")]
    FrameTooLarge { limit: usize },

    #[error("connection closed in the middle of a frame")]
    IncompleteFrame,

    #[error("unexpected frame: expected {expected}")]
    UnexpectedFrame { expected: &'static str },

    #[error("the edge core rejected the connection")]
    Rejected { code: String, message: String },

    #[error("protocol version mismatch: client speaks {client}, core speaks {core}")]
    ProtocolVersionMismatch { client: u32, core: u32 },

    #[error("local credential is unavailable: {reason}")]
    Credential { reason: String },

    #[error("endpoint path is too long for this platform ({length} > {limit}): {path}")]
    EndpointPathTooLong {
        path: String,
        length: usize,
        limit: usize,
    },

    #[error("could not prepare the runtime directory `{path}`: {source}")]
    RuntimeDirectory {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl IpcError {
    /// Stable code for clients and logs.
    pub fn code(&self) -> &str {
        match self {
            IpcError::NotConnected { .. }
            | IpcError::ConnectionLost
            | IpcError::Io(_)
            | IpcError::IncompleteFrame => error_codes::CORE_UNAVAILABLE,
            IpcError::Codec(_)
            | IpcError::FrameTooLarge { .. }
            | IpcError::UnexpectedFrame { .. } => error_codes::MALFORMED_FRAME,
            IpcError::Rejected { code, .. } => code,
            IpcError::ProtocolVersionMismatch { .. } => error_codes::PROTOCOL_VERSION_MISMATCH,
            IpcError::Credential { .. } => error_codes::AUTHENTICATION_FAILED,
            IpcError::EndpointPathTooLong { .. } | IpcError::RuntimeDirectory { .. } => {
                error_codes::CORE_UNAVAILABLE
            }
        }
    }

    /// True when the core may simply be down, so a client should back off and
    /// retry rather than surface a hard failure.
    pub fn is_core_unavailable(&self) -> bool {
        self.code() == error_codes::CORE_UNAVAILABLE
    }
}
