//! Local client authentication.
//!
//! Threat model: another process on the same device sending commands to the
//! register — fake sales, phantom cash payments, opening or closing the
//! business day, reading the journal. Root and physical access are explicitly
//! out of scope.
//!
//! Two layers:
//!
//! 1. **OS isolation** — the runtime directory is `0700` and the socket `0600`,
//!    so only processes of the POS user can connect at all.
//! 2. **A local credential** — a 256-bit token presented once per connection in
//!    the `HELLO` frame and compared in constant time. This layer survives
//!    transports whose OS isolation is weaker (Android, a future TCP fallback)
//!    and makes the daemon's trust boundary explicit instead of implied.
//!
//! The token authenticates a *process*, not a cashier. Operator authentication
//! is a separate concern.
//!
//! Deliberately not in this phase: expiry, automatic or remote rotation,
//! certificates, mTLS. Rotation is manual: delete the token file and restart
//! the daemon; clients re-read it on every connect.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::IpcError;

/// Token length in bytes before hex encoding.
const TOKEN_BYTES: usize = 32;

/// A local credential. Never `Display`, never logged, redacted in `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthToken(String);

impl AuthToken {
    /// Generate a fresh 256-bit token from the OS CSPRNG.
    pub fn generate() -> Result<Self, IpcError> {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| IpcError::Credential {
            reason: format!("could not read from the system RNG: {error}"),
        })?;
        let mut hex = String::with_capacity(TOKEN_BYTES * 2);
        for byte in bytes {
            hex.push_str(&format!("{byte:02x}"));
        }
        Ok(Self(hex))
    }

    /// Adopt an existing credential, e.g. one read from platform storage by a
    /// non-file [`LocalCredentialProvider`]. Validates the format so a
    /// truncated or mangled value fails here rather than at the handshake.
    pub fn from_hex(raw: &str) -> Result<Self, IpcError> {
        Self::from_stored(raw)
    }

    fn from_stored(raw: &str) -> Result<Self, IpcError> {
        let trimmed = raw.trim();
        if trimmed.len() != TOKEN_BYTES * 2 || !trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(IpcError::Credential {
                reason: "stored credential is malformed".to_owned(),
            });
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// The wire representation. Only the transport handshake may call this.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Constant-time comparison against a candidate presented by a peer.
    pub fn verify(&self, candidate: &str) -> bool {
        constant_time_eq(self.0.as_bytes(), candidate.as_bytes())
    }
}

impl fmt::Debug for AuthToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthToken(<redacted>)")
    }
}

/// Comparison whose duration does not depend on where the first difference is.
/// `black_box` keeps the optimizer from reintroducing an early exit.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let length_differs = left.len() != right.len();
    let mut diff = u8::from(length_differs);
    let len = left.len().min(right.len());
    for index in 0..len {
        diff |= left[index] ^ right[index];
    }
    std::hint::black_box(diff) == 0 && !length_differs
}

/// How a client or the daemon obtains the local credential.
///
/// An abstraction rather than a path so that Android can later store the
/// credential in app-private storage or the Keystore without `EdgeClient`
/// changing. The desktop implementation is [`FileCredentialProvider`].
pub trait LocalCredentialProvider: fmt::Debug + Send + Sync {
    /// Read an existing credential. Used by clients.
    fn load(&self) -> Result<AuthToken, IpcError>;

    /// Read the credential, creating it on first run. Used by the daemon
    /// during provisioning.
    fn load_or_create(&self) -> Result<AuthToken, IpcError>;
}

/// Credential stored in an owner-only file inside the runtime directory.
#[derive(Debug, Clone)]
pub struct FileCredentialProvider {
    path: PathBuf,
}

impl FileCredentialProvider {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<AuthToken, IpcError> {
        let raw = std::fs::read_to_string(&self.path).map_err(|error| IpcError::Credential {
            reason: match error.kind() {
                std::io::ErrorKind::NotFound => {
                    "no local credential found; is the edge daemon provisioned?".to_owned()
                }
                _ => format!("could not read the local credential: {error}"),
            },
        })?;
        #[cfg(unix)]
        self.warn_on_loose_permissions();
        AuthToken::from_stored(&raw)
    }

    #[cfg(unix)]
    fn warn_on_loose_permissions(&self) {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(&self.path) {
            let mode = metadata.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                tracing::warn!(
                    path = %self.path.display(),
                    mode = format!("{mode:o}"),
                    "local credential file is readable beyond its owner"
                );
            }
        }
    }

    fn create(&self) -> Result<AuthToken, IpcError> {
        let token = AuthToken::generate()?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&self.path) {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(token.expose().as_bytes())
                    .and_then(|()| file.flush())
                    .map_err(|error| IpcError::Credential {
                        reason: format!("could not write the local credential: {error}"),
                    })?;
                tracing::info!(
                    path = %self.path.display(),
                    "generated local credential for this installation"
                );
                Ok(token)
            }
            // Another process won the race; its token is authoritative.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => self.read(),
            Err(error) => Err(IpcError::Credential {
                reason: format!("could not create the local credential: {error}"),
            }),
        }
    }
}

impl LocalCredentialProvider for FileCredentialProvider {
    fn load(&self) -> Result<AuthToken, IpcError> {
        self.read()
    }

    fn load_or_create(&self) -> Result<AuthToken, IpcError> {
        match self.read() {
            Ok(token) => Ok(token),
            Err(_) if !self.path.exists() => self.create(),
            Err(error) => Err(error),
        }
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_distinct_256_bit_tokens() {
        let a = AuthToken::generate().unwrap();
        let b = AuthToken::generate().unwrap();
        assert_eq!(a.expose().len(), 64);
        assert_ne!(a.expose(), b.expose());
    }

    #[test]
    fn verifies_only_the_exact_token() {
        let token = AuthToken::generate().unwrap();
        assert!(token.verify(token.expose()));
        assert!(!token.verify(""));
        assert!(!token.verify("short"));
        assert!(!token.verify(&format!("{}0", token.expose())));
        let mut tampered: Vec<char> = token.expose().chars().collect();
        tampered[0] = if tampered[0] == 'a' { 'b' } else { 'a' };
        assert!(!token.verify(&tampered.into_iter().collect::<String>()));
    }

    #[test]
    fn debug_output_never_contains_the_token() {
        let token = AuthToken::generate().unwrap();
        let rendered = format!("{token:?}");
        assert!(!rendered.contains(token.expose()));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn creates_on_first_run_and_is_stable_afterwards() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileCredentialProvider::new(dir.path().join("daemon.token"));
        assert!(provider.load().is_err());
        let created = provider.load_or_create().unwrap();
        let loaded = provider.load().unwrap();
        assert_eq!(created.expose(), loaded.expose());
        assert_eq!(
            provider.load_or_create().unwrap().expose(),
            created.expose()
        );
    }

    #[cfg(unix)]
    #[test]
    fn credential_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let provider = FileCredentialProvider::new(dir.path().join("daemon.token"));
        provider.load_or_create().unwrap();
        let mode = std::fs::metadata(provider.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn rejects_a_malformed_stored_credential() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.token");
        std::fs::write(&path, "not-a-token").unwrap();
        let provider = FileCredentialProvider::new(&path);
        assert!(matches!(provider.load(), Err(IpcError::Credential { .. })));
    }
}
