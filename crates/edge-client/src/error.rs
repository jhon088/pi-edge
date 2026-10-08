//! Client errors.
//!
//! Three distinct situations, because a UI must react differently to each:
//! the core is not there, the core said no, or the core said something we did
//! not understand.

use edge_ipc::IpcError;
use edge_protocol::error_codes;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    /// The daemon is unreachable or the connection dropped. The caller must
    /// not invent state, and must not assume a command did or did not apply.
    #[error("the edge core is unavailable: {detail}")]
    Unavailable { detail: String },

    /// The core answered with a refusal. `code` is stable (see
    /// [`edge_protocol::error_codes`]); `message` is for humans only.
    #[error("the edge core refused the operation [{code}]: {message}")]
    Refused { code: String, message: String },

    /// Framing, versioning or credential problem: a bug or a tampered peer,
    /// not a business outcome.
    #[error("transport failure [{code}]: {message}")]
    Transport { code: String, message: String },

    /// The daemon replied with a result shape this client did not expect.
    #[error("unexpected result from the core: expected {expected}, got {actual}")]
    UnexpectedResult {
        expected: &'static str,
        actual: String,
    },
}

impl ClientError {
    pub fn code(&self) -> &str {
        match self {
            ClientError::Unavailable { .. } => error_codes::CORE_UNAVAILABLE,
            ClientError::Refused { code, .. } | ClientError::Transport { code, .. } => code,
            ClientError::UnexpectedResult { .. } => error_codes::UNEXPECTED_RESULT,
        }
    }

    /// True when the right reaction is "show disconnected and retry later".
    pub fn is_core_unavailable(&self) -> bool {
        matches!(self, ClientError::Unavailable { .. })
    }
}

impl From<IpcError> for ClientError {
    fn from(error: IpcError) -> Self {
        if error.is_core_unavailable() {
            return ClientError::Unavailable {
                detail: error.to_string(),
            };
        }
        match error {
            IpcError::Rejected { code, message } => ClientError::Transport { code, message },
            other => ClientError::Transport {
                code: other.code().to_owned(),
                message: other.to_string(),
            },
        }
    }
}
