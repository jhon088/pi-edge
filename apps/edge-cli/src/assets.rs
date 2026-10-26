//! Asset importer.
//!
//! The development stand-in for whatever eventually delivers bytes — a cloud
//! sync, a bundle, a USB stick. It reads a local file, hashes it, streams it
//! to the core in chunks, and asks the core to verify it (ADR 045).
//!
//! The CLI never says an asset is valid. It says what it is sending; PI.Edge
//! hashes the bytes it received and decides.

use std::io::Read;
use std::path::Path;

use edge_client::{EdgeClient, IpcEdgeClient};
use edge_protocol::{AssetChunkRequestV1, AssetRefV1, AssetStatusV1, MAX_ASSET_CHUNK_BYTES};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::CliError;

/// Read in slices rather than whole: an asset may be megabytes, and a CLI has
/// no more business holding one in memory than the core does.
const READ_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub struct AssetImportOutcome {
    pub status: AssetStatusV1,
    pub chunks_sent: usize,
    pub bytes_sent: i64,
    /// True when the core already had these bytes, so nothing was transferred.
    pub already_present: bool,
}

impl AssetImportOutcome {
    pub fn to_value(&self) -> Result<Value, CliError> {
        Ok(json!({
            "asset": serde_json::to_value(&self.status)
                .map_err(|e| CliError::Serialization(e.to_string()))?,
            "chunks_sent": self.chunks_sent,
            "bytes_sent": self.bytes_sent,
            "already_present": self.already_present,
        }))
    }
}

/// Hash a local file and stream it into the core's asset store.
///
/// Resumable: the core is asked what it already has, and the transfer starts
/// from there. Interrupt it and run it again — the same file produces the same
/// hash, so the same asset.
pub fn import_asset(
    client: &IpcEdgeClient,
    path: &Path,
    media_type: &str,
    chunk_bytes: usize,
) -> Result<AssetImportOutcome, CliError> {
    let (digest, byte_size) = digest_file(path)?;
    let asset = AssetRefV1 {
        content_hash: format!("sha256:{digest}"),
        media_type: media_type.to_owned(),
        byte_size,
    };

    // What the core already has decides where this starts.
    let existing = client.asset_status(asset.clone())?;
    if existing.state == "READY" {
        return Ok(AssetImportOutcome {
            status: existing,
            chunks_sent: 0,
            bytes_sent: 0,
            already_present: true,
        });
    }

    let budget = chunk_bytes.clamp(1, MAX_ASSET_CHUNK_BYTES);
    let mut file = open(path)?;
    let mut offset = 0_i64;
    let mut chunks_sent = 0_usize;
    let mut bytes_sent = 0_i64;
    let mut buffer = vec![0_u8; budget];
    loop {
        let read = fill(&mut file, &mut buffer, path)?;
        if read == 0 {
            break;
        }
        // Already staged by an earlier attempt: skip the bytes rather than
        // re-sending them. The core would accept them as a no-op anyway.
        if offset + i64::try_from(read).unwrap_or(i64::MAX) <= existing.bytes_received {
            offset += i64::try_from(read).unwrap_or(i64::MAX);
            continue;
        }
        client.put_asset_chunk(AssetChunkRequestV1::new(
            asset.clone(),
            offset,
            &buffer[..read],
        ))?;
        chunks_sent += 1;
        bytes_sent += i64::try_from(read).unwrap_or(i64::MAX);
        offset += i64::try_from(read).unwrap_or(i64::MAX);
    }

    let status = client.commit_asset(asset)?;
    Ok(AssetImportOutcome {
        status,
        chunks_sent,
        bytes_sent,
        already_present: false,
    })
}

/// SHA-256 and length of a local file, streamed.
fn digest_file(path: &Path) -> Result<(String, i64), CliError> {
    let mut file = open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; READ_BUFFER_BYTES];
    let mut length = 0_i64;
    loop {
        let read = fill(&mut file, &mut buffer, path)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        length += i64::try_from(read).unwrap_or(i64::MAX);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    Ok((hex, length))
}

fn open(path: &Path) -> Result<std::fs::File, CliError> {
    std::fs::File::open(path)
        .map_err(|error| CliError::Input(format!("cannot read {}: {error}", path.display())))
}

/// Read until the buffer is full or the file ends, so a short read never ends
/// a transfer early.
fn fill(file: &mut std::fs::File, buffer: &mut [u8], path: &Path) -> Result<usize, CliError> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = file
            .read(&mut buffer[filled..])
            .map_err(|error| CliError::Input(format!("cannot read {}: {error}", path.display())))?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}
