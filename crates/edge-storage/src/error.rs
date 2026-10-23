//! Storage errors, mapped into `AppError` at the port boundary.

use edge_application::AppError;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("sqlite failure: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("could not prepare database directory `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "another PI.Edge runtime already owns this installation \
         (lock: {path}{})", diagnostics.as_ref().map(|d| format!("; {d}")).unwrap_or_default()
    )]
    SingleWriterViolation {
        path: String,
        /// Whatever the previous owner wrote for humans. Diagnostics only.
        diagnostics: Option<String>,
    },

    /// A migration left a child row pointing at a parent that is not there.
    ///
    /// Table rebuilds run with foreign keys off — SQLite's own prescribed
    /// procedure — so this is the net that catches a rebuild which copied the
    /// rows badly. It is a bug in the migration, never an operational
    /// condition.
    #[error("migrations left orphaned rows in `{table}` referencing `{parent}`")]
    MigrationLeftOrphans { table: String, parent: String },

    #[error(
        "migration {version} ({name}) was applied with a different definition \
         (recorded checksum {recorded}, current {current})"
    )]
    MigrationChecksumMismatch {
        version: i64,
        name: &'static str,
        recorded: String,
        current: String,
    },
}

impl From<StorageError> for AppError {
    fn from(value: StorageError) -> Self {
        AppError::Storage(value.to_string())
    }
}

pub(crate) fn sqlite_err(error: rusqlite::Error) -> AppError {
    AppError::Storage(error.to_string())
}
