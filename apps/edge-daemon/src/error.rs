//! Daemon startup and runtime errors.

use edge_application::AppError;
use edge_ipc::IpcError;
use edge_storage::StorageError;

/// Exit code when another runtime already owns the installation. Distinct so
/// supervisors can tell "already running" from "crashed".
pub const EXIT_SINGLE_WRITER: i32 = 2;

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Ipc(#[from] IpcError),
    #[error(transparent)]
    App(#[from] AppError),
    #[error("io failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid configuration: {0}")]
    Configuration(String),
}

impl DaemonError {
    /// True when the failure is "another runtime owns this installation".
    pub fn is_single_writer_violation(&self) -> bool {
        matches!(
            self,
            DaemonError::Storage(StorageError::SingleWriterViolation { .. })
        )
    }

    pub fn exit_code(&self) -> i32 {
        if self.is_single_writer_violation() {
            EXIT_SINGLE_WRITER
        } else {
            1
        }
    }
}
