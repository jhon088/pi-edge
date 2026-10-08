//! Runtime layout and endpoint identity.
//!
//! Everything a running installation needs lives in one private directory next
//! to the database, so one database means one runtime:
//!
//! ```text
//! <db-dir>/pi-edge.db           the database (owned by the daemon)
//! <db-dir>/.pi-edge/            runtime directory, 0700
//! <db-dir>/.pi-edge/d.sock      IPC endpoint, 0600 (unix)
//! <db-dir>/.pi-edge/daemon.lock single-writer lock
//! <db-dir>/.pi-edge/daemon.token local credential, 0600
//! ```
//!
//! The endpoint name is derived from the canonical database path, which gives a
//! stable identity per installation. It is **not** an authentication mechanism:
//! knowing the name gets you a connection, not a session (see
//! [`crate::credentials`]).

use std::io;
use std::path::{Path, PathBuf};

use crate::error::IpcError;

/// Unix domain socket paths are bounded by `sockaddr_un.sun_path`: 104 bytes on
/// macOS, 108 on Linux. Fail loudly rather than silently truncating.
#[cfg(unix)]
const MAX_SOCKET_PATH_LEN: usize = 100;

/// Where a client connects and a daemon listens, without naming the primitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    label: String,
    inner: EndpointInner,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EndpointInner {
    /// A path in the filesystem namespace (unix domain socket).
    Filesystem(PathBuf),
    /// A name in the OS-specific namespace (Windows named pipe).
    Namespaced(String),
}

impl Endpoint {
    pub(crate) fn filesystem(path: PathBuf) -> Self {
        Self {
            label: path.display().to_string(),
            inner: EndpointInner::Filesystem(path),
        }
    }

    #[cfg_attr(unix, allow(dead_code))]
    pub(crate) fn namespaced(name: String) -> Self {
        Self {
            label: name.clone(),
            inner: EndpointInner::Namespaced(name),
        }
    }

    /// Human-readable identity for logs and the diagnostic screen.
    pub fn label(&self) -> &str {
        &self.label
    }

    pub(crate) fn inner(&self) -> &EndpointInner {
        &self.inner
    }

    /// The backing file, when the platform uses one.
    pub(crate) fn filesystem_path(&self) -> Option<&Path> {
        match &self.inner {
            EndpointInner::Filesystem(path) => Some(path),
            EndpointInner::Namespaced(_) => None,
        }
    }
}

/// Filesystem layout derived from the database path.
#[derive(Debug, Clone)]
pub struct RuntimePaths {
    database: PathBuf,
    runtime_dir: PathBuf,
    installation_id: String,
}

impl RuntimePaths {
    /// Resolve the layout for a database path. The parent directory is created
    /// if missing so that the paths can be canonicalized.
    pub fn for_database(database: impl AsRef<Path>) -> Result<Self, IpcError> {
        let database = database.as_ref();
        let parent = match database.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        };
        std::fs::create_dir_all(&parent).map_err(|source| IpcError::RuntimeDirectory {
            path: parent.display().to_string(),
            source,
        })?;
        let canonical_parent =
            parent
                .canonicalize()
                .map_err(|source| IpcError::RuntimeDirectory {
                    path: parent.display().to_string(),
                    source,
                })?;
        let file_name = database
            .file_name()
            .ok_or_else(|| IpcError::RuntimeDirectory {
                path: database.display().to_string(),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "database path has no file name",
                ),
            })?;
        let database = canonical_parent.join(file_name);
        let runtime_dir = canonical_parent.join(".pi-edge");
        let installation_id = fnv1a64_hex(&database.to_string_lossy());
        Ok(Self {
            database,
            runtime_dir,
            installation_id,
        })
    }

    pub fn database(&self) -> &Path {
        &self.database
    }

    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    /// Stable per-installation identity, derived from the canonical database
    /// path. Used to name the endpoint; never used to authenticate.
    pub fn installation_id(&self) -> &str {
        &self.installation_id
    }

    pub fn lock_file(&self) -> PathBuf {
        self.runtime_dir.join("daemon.lock")
    }

    pub fn token_file(&self) -> PathBuf {
        self.runtime_dir.join("daemon.token")
    }

    /// Create the runtime directory with owner-only permissions.
    pub fn ensure_runtime_dir(&self) -> Result<(), IpcError> {
        std::fs::create_dir_all(&self.runtime_dir).map_err(|source| {
            IpcError::RuntimeDirectory {
                path: self.runtime_dir.display().to_string(),
                source,
            }
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.runtime_dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|source| IpcError::RuntimeDirectory {
                    path: self.runtime_dir.display().to_string(),
                    source,
                })?;
        }
        Ok(())
    }

    /// The IPC endpoint for this installation.
    #[cfg(unix)]
    pub fn endpoint(&self) -> Result<Endpoint, IpcError> {
        let path = self.runtime_dir.join("d.sock");
        let length = path.as_os_str().len();
        if length > MAX_SOCKET_PATH_LEN {
            return Err(IpcError::EndpointPathTooLong {
                path: path.display().to_string(),
                length,
                limit: MAX_SOCKET_PATH_LEN,
            });
        }
        Ok(Endpoint::filesystem(path))
    }

    /// The IPC endpoint for this installation.
    #[cfg(windows)]
    pub fn endpoint(&self) -> Result<Endpoint, IpcError> {
        Ok(Endpoint::namespaced(format!(
            "pi-edge-{}.sock",
            self.installation_id
        )))
    }
}

/// FNV-1a, hex encoded. Short, stable, dependency-free; not a security
/// primitive and not used as one.
fn fnv1a64_hex(value: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_a_private_runtime_directory_next_to_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::for_database(dir.path().join("pi-edge.db")).unwrap();
        assert_eq!(paths.runtime_dir().file_name().unwrap(), ".pi-edge");
        assert!(paths.lock_file().starts_with(paths.runtime_dir()));
        assert!(paths.token_file().starts_with(paths.runtime_dir()));
        paths.ensure_runtime_dir().unwrap();
        assert!(paths.runtime_dir().is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn runtime_directory_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::for_database(dir.path().join("pi-edge.db")).unwrap();
        paths.ensure_runtime_dir().unwrap();
        let mode = std::fs::metadata(paths.runtime_dir())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn installation_id_is_stable_and_path_dependent() {
        let dir = tempfile::tempdir().unwrap();
        let a = RuntimePaths::for_database(dir.path().join("pi-edge.db")).unwrap();
        let again = RuntimePaths::for_database(dir.path().join("pi-edge.db")).unwrap();
        let other = RuntimePaths::for_database(dir.path().join("other.db")).unwrap();
        assert_eq!(a.installation_id(), again.installation_id());
        assert_ne!(a.installation_id(), other.installation_id());
        assert_eq!(a.installation_id().len(), 16);
    }

    #[test]
    fn endpoint_label_is_derived_from_the_installation() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::for_database(dir.path().join("pi-edge.db")).unwrap();
        let endpoint = paths.endpoint().unwrap();
        assert!(!endpoint.label().is_empty());
    }
}
