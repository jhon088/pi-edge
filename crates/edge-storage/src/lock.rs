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
//! Who owns it is written **beside** the lock, not inside it. That is not a
//! stylistic choice: Windows locks a byte range, so a second process cannot
//! read the bytes of a file the first one holds — `ERROR_LOCK_VIOLATION`,
//! not a permission problem. Unix locks are advisory and let the read
//! through, which is why writing diagnostics into the locked file worked
//! there and silently produced nothing on Windows.
//!
//! Either way the owner file is **diagnostics only**. The pid in it is never
//! consulted to decide whether the lock is free; only the OS lock is
//! authoritative.

use std::fs::{File, OpenOptions};
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

    /// Where the owner's identity is recorded: beside the lock, never in it.
    fn owner_path(lock: &Path) -> PathBuf {
        lock.with_extension("owner")
    }

    /// Record who owns the lock, for humans reading the directory and for the
    /// next runtime's refusal message. Never used as lock state.
    ///
    /// Written to a sibling file rather than into the lock, so that a process
    /// refused the lock can still read it. See the module note.
    pub fn write_diagnostics(
        &mut self,
        pid: u32,
        endpoint: &str,
        started_at: &str,
    ) -> Result<(), StorageError> {
        let body = format!(
            "# PI.Edge single-writer owner — diagnostics only, not lock state.\n\
             pid = {pid}\n\
             endpoint = {endpoint}\n\
             started_at = {started_at}\n"
        );
        let owner = Self::owner_path(&self.path);
        std::fs::write(&owner, body).map_err(|source| StorageError::Io {
            path: owner.display().to_string(),
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

fn read_diagnostics(lock: &Path) -> Option<String> {
    // The owner file, never the lock file: on Windows the lock covers the
    // whole byte range and reading it from here would fail.
    let body = std::fs::read_to_string(SingleWriterLock::owner_path(lock)).ok()?;
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

    /// The refusal has to name the owner, on every platform.
    ///
    /// This is what Windows failed: diagnostics used to live *inside* the
    /// locked file, and Windows locks a byte range, so the process being
    /// refused could not read them. The owner file is a sibling precisely so
    /// that the refused process can.
    #[test]
    fn the_owner_is_readable_by_the_runtime_being_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");

        let mut held = SingleWriterLock::acquire(&path).unwrap();
        held.write_diagnostics(4242, "endpoint", "2026-10-26T00:00:00Z")
            .unwrap();

        // Readable while the lock is held — the whole point.
        let owner = SingleWriterLock::owner_path(&path);
        let body = std::fs::read_to_string(&owner).expect("the owner file is readable");
        assert!(body.contains("4242"));
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap_or_default()
                .contains("4242"),
            "diagnostics must not be written into the locked file"
        );

        match SingleWriterLock::acquire(&path) {
            Err(StorageError::SingleWriterViolation {
                diagnostics,
                path: named,
            }) => {
                let diagnostics = diagnostics.expect("the refusal names the owner");
                assert!(diagnostics.contains("4242"), "no pid: {diagnostics}");
                assert!(named.contains("daemon.lock"), "no lock path: {named}");
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
        std::fs::write(SingleWriterLock::owner_path(&path), "pid = 999999\n").unwrap();
        // A pid left by a dead process must not block acquisition.
        let _held = SingleWriterLock::acquire(&path).unwrap();
    }
}
