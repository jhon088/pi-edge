//! CLI-level error type. Keeps the exit path typed instead of panicking.

use edge_client::ClientError;

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("output serialization failed: {0}")]
    Serialization(String),
    #[error("{0}")]
    Input(String),
}

impl CliError {
    /// Stable machine-readable discriminator, printed on stderr.
    pub fn kind(&self) -> &str {
        match self {
            CliError::Client(error) => error.code(),
            CliError::Serialization(_) => "SERIALIZATION_FAILURE",
            CliError::Input(_) => "INVALID_INPUT",
        }
    }

    /// Hint shown when the daemon is simply not running.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            CliError::Client(error) if error.is_core_unavailable() => {
                Some("start the runtime with `cargo run -p edge-daemon -- --db <path>`")
            }
            _ => None,
        }
    }
}
