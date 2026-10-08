//! Single-writer runtime lock.
//!
//! SQLite's own locking prevents interleaved writes, but it does not give us a
//! single *runtime identity*: two processes could each hold a consistent view
//! and still make conflicting decisions about the same register (two
//! sequences, two notions of the active transaction, two outbox writers).
//!
//! So one OS-level exclusive lock per installation guards the whole runtime.
//! The lock is held by the operating system on an open file descriptor, which
//! means it is released automatically when the process dies — including a
//! `SIGKILL` or a power cut. There is no stale-lock cleanup to get wrong.
//!
//! The lock file's contents are **diagnostics only**. The pid written inside is
//! never consulted to decide whether the lock is free; only the OS lock is
//! authoritative.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::error::StorageError;

/// An acquired exclusive runtime lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct SingleWriterLock {
    file: File,
    path: PathBuf,
}

impl SingleWriterLock {
    /// Try to become the single writer for this installation.
    ///
    /// Must be called **before** opening the database: if another runtime owns
    /// the installation, this process must not touch the files at all.
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| StorageError::Io {
                path: path.display().to_string(),
                source,
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file, path }),
            Err(std::fs::TryLockError::WouldBlock) => {
                let diagnostics = read_diagnostics(&path);
                Err(StorageError::SingleWriterViolation {
                    path: path.display().to_string(),
                    diagnostics,
                })
            }
            Err(std::fs::TryLockError::Error(source)) => Err(StorageError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Record who owns the lock, for humans reading the directory. Never used
    /// as lock state.
    pub fn write_diagnostics(
        &mut self,
        pid: u32,
        endpoint: &str,
        started_at: &str,
    ) -> Result<(), StorageError> {
        let body = format!(
            "# PI.Edge single-writer lock — diagnostics only, not lock state.\n\
             pid = {pid}\n\
             endpoint = {endpoint}\n\
             started_at = {started_at}\n"
        );
        let mut write = || -> std::io::Result<()> {
            self.file.seek(SeekFrom::Start(0))?;
            self.file.set_len(0)?;
            self.file.write_all(body.as_bytes())?;
            self.file.flush()
        };
        write().map_err(|source| StorageError::Io {
            path: self.path.display().to_string(),
            source,
        })
    }
}

impl Drop for SingleWriterLock {
    fn drop(&mut self) {
        // The OS releases the lock when the descriptor closes; unlocking
        // explicitly keeps the intent visible. The file itself is left in
        // place: deleting it would race with another starting runtime.
        if let Err(error) = self.file.unlock() {
            tracing::warn!(path = %self.path.display(), %error, "could not release the runtime lock");
        }
    }
}

fn read_diagnostics(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut body = String::new();
    file.read_to_string(&mut body).ok()?;
    let summary: Vec<&str> = body
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .collect();
    if summary.is_empty() {
        None
    } else {
        Some(summary.join(", "))
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_runtime_can_hold_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");

        let mut first = SingleWriterLock::acquire(&path).unwrap();
        first
            .write_diagnostics(4242, "/tmp/pi-edge/d.sock", "2026-10-08T15:00:00Z")
            .unwrap();

        let second = SingleWriterLock::acquire(&path);
        match second {
            Err(StorageError::SingleWriterViolation { diagnostics, .. }) => {
                let diagnostics = diagnostics.unwrap_or_default();
                assert!(diagnostics.contains("4242"), "{diagnostics}");
            }
            other => panic!("expected a single-writer violation, got {other:?}"),
        }
    }

    #[test]
    fn releasing_the_lock_lets_the_next_runtime_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");
        {
            let _held = SingleWriterLock::acquire(&path).unwrap();
            assert!(SingleWriterLock::acquire(&path).is_err());
        }
        // Previous owner gone: the lock is free again, with no cleanup step.
        let _again = SingleWriterLock::acquire(&path).unwrap();
    }

    #[test]
    fn diagnostics_are_not_required_to_acquire() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");
        std::fs::write(&path, "pid = 999999\n").unwrap();
        // A pid in the file from a dead process must not block acquisition.
        let _held = SingleWriterLock::acquire(&path).unwrap();
    }
}
