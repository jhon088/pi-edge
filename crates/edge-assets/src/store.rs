//! The local content-addressed asset store.
//!
//! ```text
//! <root>/staging/<hex>.part      bytes still arriving; never served
//! <root>/sha256/<ab>/<hex>       verified bytes; immutable, shareable
//! ```
//!
//! The identity of an asset is the SHA-256 of its bytes and nothing else. Not
//! the filename it had upstream, not the URL it came from, not the product
//! that happens to reference it — so two snapshots that show the same photo
//! share one file, and re-importing it is free.
//!
//! The lifecycle is the one the catalog already uses, for the same reason:
//!
//! ```text
//! write_chunk  xN   bytes accumulate in staging, resumable, never served
//! commit            size, SHA-256 and media type verified, then promoted
//!                   into the content store with one atomic rename
//! ```
//!
//! **An unverified byte is never visible.** Promotion is a rename inside one
//! filesystem, so a reader either sees the whole verified file or no file at
//! all, whatever happens to the process in between.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use edge_catalog::AssetRef;
use sha2::{Digest, Sha256};

use crate::error::AssetError;

/// Default ceiling on one asset, overridable by the composition root.
///
/// A product photo is tens of kilobytes and a screensaver is a few megabytes;
/// 32 MiB leaves room for both without letting one declaration fill a till's
/// disk.
pub const DEFAULT_MAX_ASSET_BYTES: i64 = 32 * 1024 * 1024;

/// How much of a file is read at a time while hashing or copying, so a large
/// asset never becomes a large allocation.
const STREAM_BUFFER_BYTES: usize = 64 * 1024;

/// Media types this runtime is willing to store.
///
/// An allowlist rather than a pattern: the store serves these bytes to a
/// kiosk eventually, and "anything with a slash in it" is not a decision
/// anybody made. A type that is missing here is a deliberate addition, not an
/// accident (ADR 045).
const ACCEPTED_MEDIA_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/avif",
    "video/mp4",
    "video/webm",
];

/// What this installation has of one asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetState {
    /// Nothing, or nothing that has started arriving.
    Missing,
    /// Bytes are accumulating. Never served, and never counted as present.
    Staged { bytes_received: i64 },
    /// Present, and consistent with its declaration as far as a cheap check
    /// can tell. Only [`LocalAssetStore::verify`] re-reads the bytes.
    Ready,
    /// Present but **not** what was declared — the stored length disagrees
    /// with the declared length, or a verification pass failed. Reported
    /// rather than silently deleted, and re-importing replaces it.
    Corrupt,
}

impl AssetState {
    /// Stable wire value.
    pub const fn as_str(&self) -> &'static str {
        match self {
            AssetState::Missing => "MISSING",
            AssetState::Staged { .. } => "STAGED",
            AssetState::Ready => "READY",
            AssetState::Corrupt => "CORRUPT",
        }
    }

    /// Whether a snapshot that needs this asset may be activated.
    pub const fn is_ready(&self) -> bool {
        matches!(self, AssetState::Ready)
    }

    /// How many bytes are already on disk for this asset, staged or stored.
    pub const fn bytes_received(&self) -> i64 {
        match self {
            AssetState::Staged { bytes_received } => *bytes_received,
            _ => 0,
        }
    }
}

/// Where the bytes of an [`AssetRef`] live, and how they get there.
///
/// Deliberately says nothing about *where they came from*: a cloud sync, a
/// USB stick, a bundle, a local server and a test all hand over the same
/// chunks (ADR 045). There is no downloader behind this trait and no HTTP
/// anywhere in this crate.
pub trait LocalAssetStore: std::fmt::Debug + Send + Sync {
    /// Name of the backing implementation, for diagnostics.
    fn driver(&self) -> &'static str;

    /// Cheap check: what this installation has of the asset. Does not re-read
    /// the bytes.
    fn status(&self, asset: &AssetRef) -> Result<AssetState, AssetError>;

    /// Accept bytes at `offset`. Idempotent: a chunk that is already staged is
    /// a no-op, and a chunk for an asset that is already stored is a no-op.
    fn write_chunk(
        &self,
        asset: &AssetRef,
        offset: i64,
        bytes: &[u8],
    ) -> Result<AssetState, AssetError>;

    /// Verify everything staged for this asset and promote it atomically.
    /// Idempotent: committing an asset that is already stored succeeds and
    /// writes nothing.
    fn commit(&self, asset: &AssetRef) -> Result<AssetState, AssetError>;

    /// Re-read the stored bytes and confirm they are the declared length and
    /// hash to the declared digest. What activation uses, because "the file
    /// exists" is not the promise a snapshot makes.
    fn verify(&self, asset: &AssetRef) -> Result<(), AssetError>;

    /// Stream the stored bytes. Never the whole asset in memory, and never a
    /// path: a caller receives bytes, not a location.
    fn open(&self, asset: &AssetRef) -> Result<Box<dyn Read + Send>, AssetError>;

    /// One slice of the stored bytes, for a client that has to receive them
    /// over a framed transport. Reads only what was asked for, so a large
    /// asset is never a large allocation (ADR 045).
    fn read_at(&self, asset: &AssetRef, offset: i64, length: i64) -> Result<Vec<u8>, AssetError>;

    /// Total bytes the content store holds. Enough to report usage; no quota
    /// policy is enforced here, and nothing is ever evicted (ADR 045).
    fn stored_bytes(&self) -> Result<i64, AssetError>;
}

/// A store that has nothing and can store nothing.
///
/// What a core gets unless a composition root configured one. Every write
/// answers `ASSET_STORE_NOT_CONFIGURED` and every asset reads as `MISSING`,
/// so a snapshot that references an image cannot be activated — which is the
/// honest outcome, and the only one that keeps the non-negotiable rule true
/// (ADR 045). A retail snapshot references none and is unaffected.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnconfiguredAssetStore;

impl LocalAssetStore for UnconfiguredAssetStore {
    fn driver(&self) -> &'static str {
        "unconfigured"
    }

    fn status(&self, _asset: &AssetRef) -> Result<AssetState, AssetError> {
        Ok(AssetState::Missing)
    }

    fn write_chunk(
        &self,
        _asset: &AssetRef,
        _offset: i64,
        _bytes: &[u8],
    ) -> Result<AssetState, AssetError> {
        Err(AssetError::NotConfigured)
    }

    fn commit(&self, _asset: &AssetRef) -> Result<AssetState, AssetError> {
        Err(AssetError::NotConfigured)
    }

    fn verify(&self, asset: &AssetRef) -> Result<(), AssetError> {
        Err(AssetError::NotReady {
            content_hash: asset.content_hash.clone(),
            state: AssetState::Missing.as_str(),
        })
    }

    fn open(&self, asset: &AssetRef) -> Result<Box<dyn Read + Send>, AssetError> {
        Err(AssetError::NotReady {
            content_hash: asset.content_hash.clone(),
            state: AssetState::Missing.as_str(),
        })
    }

    fn read_at(&self, asset: &AssetRef, _offset: i64, _length: i64) -> Result<Vec<u8>, AssetError> {
        Err(AssetError::NotReady {
            content_hash: asset.content_hash.clone(),
            state: AssetState::Missing.as_str(),
        })
    }

    fn stored_bytes(&self) -> Result<i64, AssetError> {
        Err(AssetError::NotConfigured)
    }
}

/// The real store: one directory, two subdirectories, no database.
///
/// There is deliberately **no table of asset state anywhere**. The content
/// store *is* the state: a file at the path its own hash dictates, of the
/// declared length. That is what makes the crash windows converge — a process
/// that died after promoting but before "recording" anything has nothing left
/// to record, and the next attempt simply finds the file (ADR 045).
#[derive(Debug, Clone)]
pub struct FileSystemAssetStore {
    root: PathBuf,
    max_asset_bytes: i64,
}

impl FileSystemAssetStore {
    /// Open — and create — the store rooted at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, AssetError> {
        Self::with_limit(root, DEFAULT_MAX_ASSET_BYTES)
    }

    pub fn with_limit(root: impl Into<PathBuf>, max_asset_bytes: i64) -> Result<Self, AssetError> {
        let root = root.into();
        let store = Self {
            root,
            max_asset_bytes: max_asset_bytes.max(1),
        };
        create_dir(&store.content_root())?;
        create_dir(&store.staging_root())?;
        Ok(store)
    }

    pub const fn max_asset_bytes(&self) -> i64 {
        self.max_asset_bytes
    }

    fn content_root(&self) -> PathBuf {
        self.root.join("sha256")
    }

    fn staging_root(&self) -> PathBuf {
        self.root.join("staging")
    }

    /// `<root>/sha256/<ab>/<hex>`.
    ///
    /// The only input is the 64-character lowercase hex digest, re-validated
    /// here however it arrived. Nothing a client ever said — a filename, a
    /// URL, a label — reaches a path component, so there is no string to
    /// traverse with (ADR 045).
    fn content_path(&self, digest: &str) -> PathBuf {
        self.content_root().join(&digest[..2]).join(digest)
    }

    fn staging_path(&self, digest: &str) -> PathBuf {
        self.staging_root().join(format!("{digest}.part"))
    }

    /// Validate the declaration, and answer with the digest to key paths by.
    fn accept(&self, asset: &AssetRef) -> Result<String, AssetError> {
        let digest = digest_hex(&asset.content_hash)?;
        if asset.byte_size > self.max_asset_bytes {
            return Err(AssetError::TooLarge {
                content_hash: asset.content_hash.clone(),
                declared: asset.byte_size,
                limit: self.max_asset_bytes,
            });
        }
        if !ACCEPTED_MEDIA_TYPES.contains(&asset.media_type.as_str()) {
            return Err(AssetError::Rejected {
                content_hash: asset.content_hash.clone(),
                reason: format!(
                    "`{}` is not a media type this store holds",
                    asset.media_type
                ),
            });
        }
        Ok(digest)
    }

    /// Throw away a staged file that is demonstrably wrong.
    ///
    /// Only ever a `.part` file, and only when its bytes have been proven not
    /// to be the asset they claim to be. **The content store is never touched
    /// here**: a bad staging attempt must not cost an installation an asset it
    /// already has (ADR 045). A `.part` that cannot be removed is inert and
    /// the next attempt overwrites it, so the failure is not propagated over
    /// the real error that caused the discard.
    fn discard_staging(&self, digest: &str) {
        let _ = std::fs::remove_file(self.staging_path(digest));
    }
}

impl LocalAssetStore for FileSystemAssetStore {
    fn driver(&self) -> &'static str {
        "filesystem"
    }

    fn status(&self, asset: &AssetRef) -> Result<AssetState, AssetError> {
        let digest = digest_hex(&asset.content_hash)?;
        if let Some(length) = file_len(&self.content_path(&digest))? {
            return Ok(if length == asset.byte_size {
                AssetState::Ready
            } else {
                AssetState::Corrupt
            });
        }
        match file_len(&self.staging_path(&digest))? {
            Some(length) => Ok(AssetState::Staged {
                bytes_received: length,
            }),
            None => Ok(AssetState::Missing),
        }
    }

    fn write_chunk(
        &self,
        asset: &AssetRef,
        offset: i64,
        bytes: &[u8],
    ) -> Result<AssetState, AssetError> {
        let digest = self.accept(asset)?;
        if offset < 0 {
            return Err(AssetError::ChunkOutOfOrder {
                content_hash: asset.content_hash.clone(),
                expected: 0,
                offset,
            });
        }
        // Already stored and consistent: the bytes are not needed, and saying
        // so is what makes a replayed import free.
        if self.status(asset)? == AssetState::Ready {
            return Ok(AssetState::Ready);
        }

        let path = self.staging_path(&digest);
        let staged = file_len(&path)?.unwrap_or(0);
        let written = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
        if offset + written > asset.byte_size {
            // Overshooting the declaration can only produce a file that
            // hashes to nothing, so it is refused before anything is written.
            return Err(AssetError::SizeMismatch {
                content_hash: asset.content_hash.clone(),
                declared: asset.byte_size,
                actual: offset + written,
            });
        }
        if offset + written <= staged {
            // A redelivery of bytes already on disk. Accepted as a no-op: the
            // digest at commit is the authority on whether they were right,
            // and a retrying transport must not be punished for retrying.
            return Ok(AssetState::Staged {
                bytes_received: staged,
            });
        }
        if offset != staged {
            return Err(AssetError::ChunkOutOfOrder {
                content_hash: asset.content_hash.clone(),
                expected: staged,
                offset,
            });
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| io_error("open staging", &asset.content_hash, error))?;
        file.write_all(bytes)
            .map_err(|error| io_error("write staging", &asset.content_hash, error))?;
        file.flush()
            .map_err(|error| io_error("flush staging", &asset.content_hash, error))?;
        Ok(AssetState::Staged {
            bytes_received: staged + written,
        })
    }

    fn commit(&self, asset: &AssetRef) -> Result<AssetState, AssetError> {
        let digest = self.accept(asset)?;
        let content = self.content_path(&digest);

        // Already promoted. Either a replayed commit or a process that died
        // between the rename and whatever it meant to do next; both converge
        // here, because the file is the only state there is.
        if let Some(length) = file_len(&content)? {
            if length == asset.byte_size {
                self.discard_staging(&digest);
                return Ok(AssetState::Ready);
            }
        }

        let staging = self.staging_path(&digest);
        let staged = match file_len(&staging)? {
            Some(length) => length,
            None => {
                return Err(AssetError::NotStaged {
                    content_hash: asset.content_hash.clone(),
                })
            }
        };
        if staged != asset.byte_size {
            // Too long is wrong and is thrown away; too short is simply not
            // finished, and the client may send the rest.
            if staged > asset.byte_size {
                self.discard_staging(&digest);
            }
            return Err(AssetError::SizeMismatch {
                content_hash: asset.content_hash.clone(),
                declared: asset.byte_size,
                actual: staged,
            });
        }

        let (computed, prefix) = digest_file(&staging, &asset.content_hash)?;
        if computed != digest {
            // The bytes are wrong, so they are discarded: keeping them would
            // leave an import that can never succeed and never be retried.
            self.discard_staging(&digest);
            return Err(AssetError::DigestMismatch {
                declared: asset.content_hash.clone(),
                computed: format!("sha256:{computed}"),
            });
        }
        if let Some(reason) = media_type_disagreement(&asset.media_type, &prefix) {
            self.discard_staging(&digest);
            return Err(AssetError::Rejected {
                content_hash: asset.content_hash.clone(),
                reason,
            });
        }

        // Durable before visible: the staged bytes reach the disk, and only
        // then does the rename make them readable under their hash.
        durable::sync_file(&staging)
            .map_err(|error| io_error("sync staging", &asset.content_hash, error))?;
        if let Some(shard) = content.parent() {
            create_dir(shard)?;
        }
        std::fs::rename(&staging, &content)
            .map_err(|error| io_error("promote asset", &asset.content_hash, error))?;
        durable::sync_parent_directory(content.parent());
        Ok(AssetState::Ready)
    }

    fn verify(&self, asset: &AssetRef) -> Result<(), AssetError> {
        let digest = digest_hex(&asset.content_hash)?;
        let content = self.content_path(&digest);
        let length = file_len(&content)?.ok_or_else(|| AssetError::NotReady {
            content_hash: asset.content_hash.clone(),
            state: AssetState::Missing.as_str(),
        })?;
        if length != asset.byte_size {
            return Err(AssetError::SizeMismatch {
                content_hash: asset.content_hash.clone(),
                declared: asset.byte_size,
                actual: length,
            });
        }
        let (computed, _) = digest_file(&content, &asset.content_hash)?;
        if computed != digest {
            return Err(AssetError::DigestMismatch {
                declared: asset.content_hash.clone(),
                computed: format!("sha256:{computed}"),
            });
        }
        Ok(())
    }

    fn open(&self, asset: &AssetRef) -> Result<Box<dyn Read + Send>, AssetError> {
        let state = self.status(asset)?;
        if !state.is_ready() {
            return Err(AssetError::NotReady {
                content_hash: asset.content_hash.clone(),
                state: state.as_str(),
            });
        }
        let digest = digest_hex(&asset.content_hash)?;
        let file = File::open(self.content_path(&digest))
            .map_err(|error| io_error("open asset", &asset.content_hash, error))?;
        Ok(Box::new(file))
    }

    fn read_at(&self, asset: &AssetRef, offset: i64, length: i64) -> Result<Vec<u8>, AssetError> {
        let state = self.status(asset)?;
        if !state.is_ready() {
            return Err(AssetError::NotReady {
                content_hash: asset.content_hash.clone(),
                state: state.as_str(),
            });
        }
        if offset < 0 || length < 0 {
            return Err(AssetError::Rejected {
                content_hash: asset.content_hash.clone(),
                reason: "a read is a non-negative offset and length".to_owned(),
            });
        }
        let digest = digest_hex(&asset.content_hash)?;
        let mut file = File::open(self.content_path(&digest))
            .map_err(|error| io_error("open asset", &asset.content_hash, error))?;
        file.seek(SeekFrom::Start(offset.unsigned_abs()))
            .map_err(|error| io_error("seek asset", &asset.content_hash, error))?;
        let wanted = usize::try_from(length).unwrap_or(usize::MAX);
        let mut bytes = Vec::new();
        file.take(length.unsigned_abs())
            .read_to_end(&mut bytes)
            .map_err(|error| io_error("read asset", &asset.content_hash, error))?;
        bytes.truncate(wanted);
        Ok(bytes)
    }

    fn stored_bytes(&self) -> Result<i64, AssetError> {
        let mut total: i64 = 0;
        let shards = match std::fs::read_dir(self.content_root()) {
            Ok(shards) => shards,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(io_error("list content store", "", error)),
        };
        for shard in shards {
            let shard = shard.map_err(|error| io_error("list content store", "", error))?;
            let entries = match std::fs::read_dir(shard.path()) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(io_error("list content store", "", error)),
            };
            for entry in entries {
                let entry = entry.map_err(|error| io_error("list content store", "", error))?;
                let metadata = entry
                    .metadata()
                    .map_err(|error| io_error("measure asset", "", error))?;
                if metadata.is_file() {
                    total = total.saturating_add(i64::try_from(metadata.len()).unwrap_or(i64::MAX));
                }
            }
        }
        Ok(total)
    }
}

/// The 64 hex characters of a `sha256:…` content hash.
///
/// Re-validated here even though [`AssetRef`] validated it on the way in: this
/// is the function that turns a string into a path, so it is the function that
/// has to be certain (ADR 045).
fn digest_hex(content_hash: &str) -> Result<String, AssetError> {
    let reject = |reason: &str| AssetError::Rejected {
        content_hash: content_hash.to_owned(),
        reason: reason.to_owned(),
    };
    let hex = content_hash
        .strip_prefix("sha256:")
        .ok_or_else(|| reject("a content hash must be `sha256:<64 hex>`"))?;
    if hex.len() != 64 {
        return Err(reject("a sha-256 digest is exactly 64 hex characters"));
    }
    if !hex
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(reject("a digest is lowercase hex and nothing else"));
    }
    Ok(hex.to_owned())
}

fn create_dir(path: &Path) -> Result<(), AssetError> {
    std::fs::create_dir_all(path).map_err(|error| io_error("create directory", "", error))
}

/// Length of a file, or `None` when it is not there.
fn file_len(path: &Path) -> Result<Option<i64>, AssetError> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            Ok(Some(i64::try_from(metadata.len()).unwrap_or(i64::MAX)))
        }
        Ok(_) => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error("inspect asset", "", error)),
    }
}

/// Stream a file through SHA-256, keeping its first bytes for the media-type
/// check. Never holds more than one buffer, whatever the asset's size.
fn digest_file(path: &Path, content_hash: &str) -> Result<(String, Vec<u8>), AssetError> {
    let mut file = File::open(path).map_err(|error| io_error("read asset", content_hash, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    let mut prefix = Vec::new();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| io_error("read asset", content_hash, error))?;
        if read == 0 {
            break;
        }
        if prefix.len() < 16 {
            let wanted = (16 - prefix.len()).min(read);
            prefix.extend_from_slice(&buffer[..wanted]);
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    Ok((hex, prefix))
}

/// Basic coherence between the declared media type and the bytes.
///
/// Signature checks only — no decoding, no resizing, no transcoding, and no
/// opinion about whether the image is any good. A container we cannot cheaply
/// recognise is accepted on its declaration, which is stated rather than
/// implied (ADR 045).
fn media_type_disagreement(media_type: &str, prefix: &[u8]) -> Option<String> {
    let starts = |magic: &[u8]| prefix.len() >= magic.len() && &prefix[..magic.len()] == magic;
    let matches = match media_type {
        "image/png" => starts(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
        "image/jpeg" => starts(&[0xff, 0xd8, 0xff]),
        "image/gif" => starts(b"GIF87a") || starts(b"GIF89a"),
        "image/webp" => starts(b"RIFF") && prefix.len() >= 12 && &prefix[8..12] == b"WEBP",
        // avif and the video containers are ISO-BMFF or Matroska; recognising
        // them properly means parsing boxes, which is not basic coherence.
        _ => true,
    };
    if matches {
        None
    } else {
        Some(format!(
            "the bytes are not {media_type}: the file does not start like one"
        ))
    }
}

/// Getting bytes onto the device, which is the one part of this store that
/// POSIX and Windows do not agree on.
///
/// Both operations live here together so the difference is readable in one
/// place rather than spread through `AssetStore` as scattered `cfg`s.
mod durable {
    use std::fs::OpenOptions;
    use std::io;
    use std::path::Path;

    /// Flush a file's bytes to the device.
    ///
    /// Opened for **writing**, which looks redundant for something that only
    /// flushes and is not. `fsync(2)` is defined for any open descriptor, so
    /// a read handle is enough on Unix — but its Windows counterpart,
    /// `FlushFileBuffers`, requires the handle to carry `GENERIC_WRITE` and
    /// answers `ERROR_ACCESS_DENIED` to a read-only one. Asking for write
    /// access satisfies both platforms with a single open mode, and truncates
    /// nothing: only `truncate(true)` would.
    pub fn sync_file(path: &Path) -> io::Result<()> {
        OpenOptions::new().write(true).open(path)?.sync_all()
    }

    /// Flush the directory entry a rename just created.
    ///
    /// Best effort on purpose, on every platform: if the entry is lost to
    /// power failure the bytes are simply missing, and because the store is
    /// addressed by content hash the next import re-stages the identical
    /// bytes and converges. Nothing is corrupted either way — the rename is
    /// atomic, so a reader sees the whole file or no file.
    #[cfg(unix)]
    pub fn sync_parent_directory(path: Option<&Path>) {
        use std::fs::File;
        if let Some(path) = path {
            // POSIX: a directory is openable as a file and `fsync` on it
            // commits the entries within.
            if let Ok(directory) = File::open(path) {
                let _ = directory.sync_all();
            }
        }
    }

    /// Windows has no directory flush, so this is deliberately nothing.
    ///
    /// A directory handle cannot even be opened without
    /// `FILE_FLAG_BACKUP_SEMANTICS`, and `FlushFileBuffers` does not apply to
    /// one. What stands in for it is NTFS: metadata operations go through the
    /// journal, so the rename either happened or did not. `std::fs::rename`
    /// maps to `MoveFileExW` without `MOVEFILE_WRITE_THROUGH`, so the entry
    /// is not forced to the device before the call returns — the same
    /// exposure the Unix path has if its `fsync` fails, and the same recovery
    /// answers it: re-import the bytes, which are addressed by their hash.
    ///
    /// Written out rather than left as a silently-failing `File::open`, which
    /// is what this was and which hid the difference.
    #[cfg(not(unix))]
    pub fn sync_parent_directory(_path: Option<&Path>) {}
}

fn io_error(operation: &'static str, content_hash: &str, error: io::Error) -> AssetError {
    if error.kind() == io::ErrorKind::StorageFull {
        return AssetError::OutOfSpace {
            content_hash: content_hash.to_owned(),
        };
    }
    AssetError::Io {
        operation,
        reason: error.to_string(),
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10,
    ];

    fn store() -> (FileSystemAssetStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = FileSystemAssetStore::open(dir.path().join("assets")).expect("a store");
        (store, dir)
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        let mut hex = String::new();
        for byte in digest {
            hex.push_str(&format!("{byte:02x}"));
        }
        hex
    }

    fn asset_for(bytes: &[u8]) -> AssetRef {
        AssetRef::import(
            format!("sha256:{}", sha256_hex(bytes)),
            "image/png",
            i64::try_from(bytes.len()).unwrap(),
        )
        .expect("a valid reference")
    }

    fn import(store: &FileSystemAssetStore, bytes: &[u8]) -> AssetRef {
        let asset = asset_for(bytes);
        store.write_chunk(&asset, 0, bytes).expect("staged");
        assert_eq!(store.commit(&asset).expect("committed"), AssetState::Ready);
        asset
    }

    fn read_back(store: &FileSystemAssetStore, asset: &AssetRef) -> Vec<u8> {
        let mut bytes = Vec::new();
        store
            .open(asset)
            .expect("the asset is readable")
            .read_to_end(&mut bytes)
            .expect("read");
        bytes
    }

    #[test]
    fn a_valid_asset_stages_verifies_and_promotes() {
        let (store, _dir) = store();
        let asset = asset_for(PNG);
        assert_eq!(store.status(&asset).unwrap(), AssetState::Missing);

        store.write_chunk(&asset, 0, PNG).unwrap();
        assert_eq!(
            store.status(&asset).unwrap(),
            AssetState::Staged {
                bytes_received: PNG.len() as i64
            },
            "staged bytes are never READY"
        );
        assert!(
            store.open(&asset).is_err(),
            "nothing unverified is ever served"
        );

        assert_eq!(store.commit(&asset).unwrap(), AssetState::Ready);
        store.verify(&asset).unwrap();
        assert_eq!(read_back(&store, &asset), PNG);
        assert_eq!(store.stored_bytes().unwrap(), PNG.len() as i64);
    }

    #[test]
    fn the_declared_digest_is_the_authority() {
        let (store, _dir) = store();
        // A reference whose hash belongs to other bytes entirely.
        let lying = AssetRef::import(
            format!("sha256:{}", sha256_hex(b"something else")),
            "image/png",
            PNG.len() as i64,
        )
        .unwrap();
        store.write_chunk(&lying, 0, PNG).unwrap();
        assert!(matches!(
            store.commit(&lying),
            Err(AssetError::DigestMismatch { .. })
        ));
        // Never ready, and the bad staging is gone so a retry starts clean.
        assert_eq!(store.status(&lying).unwrap(), AssetState::Missing);
        assert_eq!(store.stored_bytes().unwrap(), 0);
    }

    #[test]
    fn a_declared_size_that_is_not_the_bytes_is_refused() {
        let (store, _dir) = store();
        let hash = format!("sha256:{}", sha256_hex(PNG));

        // Too long a declaration: the staged file is short, so it stays
        // resumable rather than being thrown away.
        let long = AssetRef::import(hash.clone(), "image/png", PNG.len() as i64 + 1).unwrap();
        store.write_chunk(&long, 0, PNG).unwrap();
        assert!(matches!(
            store.commit(&long),
            Err(AssetError::SizeMismatch { actual, declared, .. })
                if actual == PNG.len() as i64 && declared == PNG.len() as i64 + 1
        ));
        assert_eq!(
            store.status(&long).unwrap(),
            AssetState::Staged {
                bytes_received: PNG.len() as i64
            },
            "an unfinished asset is resumable, not discarded"
        );

        // Too short a declaration: the chunk itself overshoots and is refused
        // before a byte is written.
        let short = AssetRef::import(hash, "image/png", 4).unwrap();
        assert!(matches!(
            store.write_chunk(&short, 0, PNG),
            Err(AssetError::SizeMismatch { .. })
        ));
    }

    #[test]
    fn importing_the_same_asset_twice_stores_one_copy() {
        let (store, _dir) = store();
        let asset = import(&store, PNG);
        let once = store.stored_bytes().unwrap();

        // Replayed in full, by a transport that does not know it already
        // succeeded.
        assert_eq!(
            store.write_chunk(&asset, 0, PNG).unwrap(),
            AssetState::Ready
        );
        assert_eq!(store.commit(&asset).unwrap(), AssetState::Ready);
        assert_eq!(store.stored_bytes().unwrap(), once, "no second copy");

        // And a second AssetRef for the same bytes — a different product
        // showing the same photo — is the same file.
        let shared =
            AssetRef::import(asset.content_hash.clone(), "image/png", asset.byte_size).unwrap();
        assert_eq!(store.status(&shared).unwrap(), AssetState::Ready);
        assert_eq!(store.stored_bytes().unwrap(), once);
    }

    #[test]
    fn chunk_boundaries_do_not_change_identity() {
        let bytes: Vec<u8> = PNG
            .iter()
            .copied()
            .chain((0..5_000_u32).map(|index| (index % 251) as u8))
            .collect();

        let mut hashes = Vec::new();
        for size in [1_usize, 7, 512, 4_096, bytes.len()] {
            let (store, _dir) = store();
            let asset = asset_for(&bytes);
            let mut offset = 0_i64;
            for chunk in bytes.chunks(size) {
                store.write_chunk(&asset, offset, chunk).unwrap();
                offset += chunk.len() as i64;
            }
            assert_eq!(store.commit(&asset).unwrap(), AssetState::Ready);
            assert_eq!(read_back(&store, &asset), bytes);
            hashes.push(asset.content_hash.clone());
        }
        assert!(
            hashes.windows(2).all(|pair| pair[0] == pair[1]),
            "the identity is the content, never how it arrived"
        );
    }

    #[test]
    fn a_gap_is_refused_and_a_replay_is_not() {
        let (store, _dir) = store();
        let asset = asset_for(PNG);
        store.write_chunk(&asset, 0, &PNG[..8]).unwrap();

        // A gap would produce a file that hashes to nothing.
        assert!(matches!(
            store.write_chunk(&asset, 9, &PNG[9..]),
            Err(AssetError::ChunkOutOfOrder {
                expected: 8,
                offset: 9,
                ..
            })
        ));
        // A redelivery of what is already staged is a no-op, which is what
        // makes an interrupted upload safe to retry.
        assert_eq!(
            store.write_chunk(&asset, 0, &PNG[..8]).unwrap(),
            AssetState::Staged { bytes_received: 8 }
        );
        store.write_chunk(&asset, 8, &PNG[8..]).unwrap();
        assert_eq!(store.commit(&asset).unwrap(), AssetState::Ready);
    }

    #[test]
    fn an_interrupted_staging_never_looks_ready() {
        let (store, _dir) = store();
        let asset = asset_for(PNG);
        store.write_chunk(&asset, 0, &PNG[..10]).unwrap();
        // Whatever the process does next, this is all a reader can see.
        assert_eq!(
            store.status(&asset).unwrap(),
            AssetState::Staged { bytes_received: 10 }
        );
        assert!(store.open(&asset).is_err());
        assert!(store.verify(&asset).is_err());
        assert!(matches!(
            store.commit(&asset),
            Err(AssetError::SizeMismatch { .. })
        ));
        assert_eq!(store.stored_bytes().unwrap(), 0);
    }

    #[test]
    fn committing_nothing_is_not_a_quiet_success() {
        let (store, _dir) = store();
        assert!(matches!(
            store.commit(&asset_for(PNG)),
            Err(AssetError::NotStaged { .. })
        ));
    }

    #[test]
    fn content_corrupted_underneath_the_store_is_detected() {
        let (store, _dir) = store();
        let asset = import(&store, PNG);

        // Same length, different bytes: only re-reading can tell.
        let digest = digest_hex(&asset.content_hash).unwrap();
        let mut tampered = PNG.to_vec();
        tampered[12] ^= 0xff;
        std::fs::write(store.content_path(&digest), &tampered).unwrap();

        assert_eq!(
            store.status(&asset).unwrap(),
            AssetState::Ready,
            "the cheap check cannot see it…"
        );
        assert!(
            matches!(store.verify(&asset), Err(AssetError::DigestMismatch { .. })),
            "…and verification can, which is why activation verifies"
        );

        // A different length is visible without re-reading.
        std::fs::write(store.content_path(&digest), b"short").unwrap();
        assert_eq!(store.status(&asset).unwrap(), AssetState::Corrupt);
        assert!(store.open(&asset).is_err());
        // Re-importing replaces it; nothing had to be deleted by hand.
        store.write_chunk(&asset, 0, PNG).unwrap();
        assert_eq!(store.commit(&asset).unwrap(), AssetState::Ready);
        store.verify(&asset).unwrap();
    }

    #[test]
    fn a_digest_can_never_become_a_path() {
        let (store, _dir) = store();
        // `AssetRef` refuses these first; the store refuses them again,
        // because it is the function that builds a path.
        for hostile in [
            "sha256:../../../../etc/passwd",
            "sha256:/etc/passwd",
            "sha256:..",
            "sha256:AAAA0000000000000000000000000000000000000000000000000000000000AA",
            "md5:0123456789abcdef0123456789abcdef",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ] {
            assert!(
                AssetRef::import(hostile.to_owned(), "image/png", 10).is_err()
                    || matches!(digest_hex(hostile), Err(AssetError::Rejected { .. })),
                "`{hostile}` must never reach a path"
            );
            assert!(matches!(
                digest_hex(hostile),
                Err(AssetError::Rejected { .. })
            ));
        }

        // And the path a valid digest produces stays inside the store.
        let asset = import(&store, PNG);
        let digest = digest_hex(&asset.content_hash).unwrap();
        let path = store.content_path(&digest);
        assert!(path.starts_with(store.content_root()));
        assert_eq!(path.components().count(), {
            let mut expected = store.content_root().components().count();
            expected += 2;
            expected
        });
    }

    #[test]
    fn a_media_type_this_store_does_not_hold_is_refused() {
        let (store, _dir) = store();
        let executable = AssetRef::import(
            format!("sha256:{}", sha256_hex(PNG)),
            "application/x-mach-binary",
            PNG.len() as i64,
        )
        .unwrap();
        assert!(matches!(
            store.write_chunk(&executable, 0, PNG),
            Err(AssetError::Rejected { .. })
        ));
        assert_eq!(store.stored_bytes().unwrap(), 0);
    }

    #[test]
    fn bytes_that_are_not_the_declared_media_type_are_refused() {
        let (store, _dir) = store();
        let not_a_png = b"GIF89a this is not a png at all";
        let mislabelled = AssetRef::import(
            format!("sha256:{}", sha256_hex(not_a_png)),
            "image/png",
            not_a_png.len() as i64,
        )
        .unwrap();
        store.write_chunk(&mislabelled, 0, not_a_png).unwrap();
        assert!(matches!(
            store.commit(&mislabelled),
            Err(AssetError::Rejected { .. })
        ));
        assert_eq!(store.stored_bytes().unwrap(), 0);

        // Correctly labelled, the same bytes are accepted.
        let honest = AssetRef::import(
            format!("sha256:{}", sha256_hex(not_a_png)),
            "image/gif",
            not_a_png.len() as i64,
        )
        .unwrap();
        store.write_chunk(&honest, 0, not_a_png).unwrap();
        assert_eq!(store.commit(&honest).unwrap(), AssetState::Ready);
    }

    #[test]
    fn an_asset_above_the_limit_is_refused_before_any_byte_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSystemAssetStore::with_limit(dir.path().join("assets"), 8).unwrap();
        let asset = asset_for(PNG);
        assert!(matches!(
            store.write_chunk(&asset, 0, PNG),
            Err(AssetError::TooLarge { limit: 8, .. })
        ));
        assert!(matches!(
            store.commit(&asset),
            Err(AssetError::TooLarge { .. })
        ));
        assert_eq!(store.stored_bytes().unwrap(), 0);
    }

    #[test]
    fn reopening_the_store_finds_what_was_already_promoted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("assets");
        let asset = {
            let store = FileSystemAssetStore::open(&root).unwrap();
            import(&store, PNG)
        };
        // A new process, a new store object, the same installation.
        let reopened = FileSystemAssetStore::open(&root).unwrap();
        assert_eq!(reopened.status(&asset).unwrap(), AssetState::Ready);
        reopened.verify(&asset).unwrap();
        assert_eq!(read_back(&reopened, &asset), PNG);
    }

    /// A process that died after the rename but before it could record
    /// anything has nothing left to record: the file is the state.
    #[test]
    fn a_promoted_asset_converges_without_any_bookkeeping() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("assets");
        let asset = asset_for(PNG);
        {
            let store = FileSystemAssetStore::open(&root).unwrap();
            store.write_chunk(&asset, 0, PNG).unwrap();
            store.commit(&asset).unwrap();
            // …and here the process dies.
        }
        let next = FileSystemAssetStore::open(&root).unwrap();
        assert_eq!(next.status(&asset).unwrap(), AssetState::Ready);
        // The retry is idempotent and writes nothing.
        assert_eq!(next.commit(&asset).unwrap(), AssetState::Ready);
        assert_eq!(next.stored_bytes().unwrap(), PNG.len() as i64);
    }

    /// A crash after the bytes were staged leaves a `.part` file, and the
    /// commit that follows the restart is the one that verifies and promotes.
    #[test]
    fn a_crash_between_staging_and_promotion_is_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("assets");
        let asset = asset_for(PNG);
        {
            let store = FileSystemAssetStore::open(&root).unwrap();
            store.write_chunk(&asset, 0, PNG).unwrap();
            // …and here the process dies, before committing.
        }
        let next = FileSystemAssetStore::open(&root).unwrap();
        assert_eq!(
            next.status(&asset).unwrap(),
            AssetState::Staged {
                bytes_received: PNG.len() as i64
            }
        );
        assert_eq!(next.commit(&asset).unwrap(), AssetState::Ready);
    }

    /// A staging attempt for one asset must never cost the installation
    /// another asset it already verified.
    #[test]
    fn a_bad_staging_never_touches_the_content_store() {
        let (store, _dir) = store();
        let good = import(&store, PNG);
        let before = store.stored_bytes().unwrap();

        let other = b"\x89PNG\r\n\x1a\x0a and other bytes entirely";
        let lying = AssetRef::import(
            format!("sha256:{}", sha256_hex(b"not these")),
            "image/png",
            other.len() as i64,
        )
        .unwrap();
        store.write_chunk(&lying, 0, other).unwrap();
        assert!(store.commit(&lying).is_err());

        assert_eq!(store.status(&good).unwrap(), AssetState::Ready);
        store.verify(&good).unwrap();
        assert_eq!(store.stored_bytes().unwrap(), before);
    }

    #[test]
    fn a_store_that_was_never_configured_has_nothing_and_says_so() {
        let store = UnconfiguredAssetStore;
        let asset = asset_for(PNG);
        assert_eq!(store.driver(), "unconfigured");
        assert_eq!(store.status(&asset).unwrap(), AssetState::Missing);
        assert!(matches!(
            store.write_chunk(&asset, 0, PNG),
            Err(AssetError::NotConfigured)
        ));
        assert!(matches!(
            store.commit(&asset),
            Err(AssetError::NotConfigured)
        ));
        assert!(matches!(
            store.verify(&asset),
            Err(AssetError::NotReady { .. })
        ));
        assert!(store.open(&asset).is_err());
        assert!(matches!(
            store.stored_bytes(),
            Err(AssetError::NotConfigured)
        ));
    }
}
