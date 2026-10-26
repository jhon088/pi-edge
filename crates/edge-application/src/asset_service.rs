//! Asset use cases: import the bytes a snapshot references, and read them.
//!
//! The lifecycle is the catalog's, applied to bytes (ADR 045):
//!
//! ```text
//! PutAssetChunk xN  -> staging, resumable, never served
//! CommitAsset       -> size + SHA-256 + media type verified, then promoted
//! ActivateCatalogSnapshot -> refuses unless every required asset is verified
//! ```
//!
//! Three things are deliberately absent.
//!
//! **No database.** Nothing here opens a transaction, because the content
//! store *is* the state: an asset is present when a file named by its own
//! digest is there, of the declared length. There is no row to keep in step
//! with the filesystem and therefore no window in which the two disagree.
//!
//! **No `request_id`.** A content-addressed write is idempotent by
//! construction — the same bytes under the same hash are the same asset — so
//! recording a command result would buy nothing a retry does not already get.
//!
//! **No transport.** Where the bytes came from is not a concern of this
//! layer: a sync, a bundle, a USB stick and a test all arrive as chunks.

use std::io::Read;

use edge_assets::AssetState;
use edge_catalog::AssetRef;
use edge_domain::prelude::CatalogSnapshotId;
use edge_payments::PaymentProviderPort;

use crate::commands::{CommitAssetCommand, PutAssetChunkCommand};
use crate::error::AppError;
use crate::ports::{Clock, EdgeStore, IdFactory, UnitOfWork};
use crate::queries::{AssetStatusView, SnapshotAssetsView};
use crate::EdgeApplication;

impl<S, C, I, P> EdgeApplication<S, C, I, P>
where
    S: EdgeStore,
    C: Clock,
    I: IdFactory,
    P: PaymentProviderPort,
{
    /// Accept one slice of an asset's bytes.
    ///
    /// The client declares the `AssetRef` with every chunk, so the size limit
    /// and the media type are checked before a byte is written and a chunk is
    /// self-describing on retry. **The client never declares the asset
    /// valid**: it says what it is sending, and PI.Edge decides (ADR 045).
    pub fn put_asset_chunk(&self, cmd: PutAssetChunkCommand) -> Result<AssetStatusView, AppError> {
        let state = self
            .assets
            .write_chunk(&cmd.asset, cmd.offset, &cmd.bytes)?;
        tracing::debug!(
            content_hash = %cmd.asset.content_hash,
            offset = cmd.offset,
            bytes = cmd.bytes.len(),
            state = state.as_str(),
            "asset chunk stored"
        );
        Ok(AssetStatusView {
            asset: cmd.asset,
            state,
        })
    }

    /// Verify everything staged for this asset and promote it.
    pub fn commit_asset(&self, cmd: CommitAssetCommand) -> Result<AssetStatusView, AppError> {
        let state = self.assets.commit(&cmd.asset)?;
        tracing::info!(
            content_hash = %cmd.asset.content_hash,
            byte_size = cmd.asset.byte_size,
            media_type = %cmd.asset.media_type,
            "asset verified and stored"
        );
        Ok(AssetStatusView {
            asset: cmd.asset,
            state,
        })
    }

    /// What this installation has of one asset. The cheap check: present and
    /// of the declared length. Verification re-reads the bytes and happens at
    /// activation.
    pub fn get_asset_status(&self, asset: AssetRef) -> Result<AssetStatusView, AppError> {
        let state = self.assets.status(&asset)?;
        Ok(AssetStatusView { asset, state })
    }

    /// Stream one asset's bytes.
    ///
    /// Works with no network of any kind: the bytes are local or the call
    /// fails saying so. **There is no fallback to a remote URL**, because a
    /// POS that quietly reaches the internet to draw a menu is a POS that
    /// stops drawing it when the line goes down (ADR 045).
    pub fn open_asset(&self, asset: &AssetRef) -> Result<Box<dyn Read + Send>, AppError> {
        Ok(self.assets.open(asset)?)
    }

    /// One slice of an asset's bytes, for a client across a framed transport.
    ///
    /// The read half of the asset runtime: a client names a content hash and
    /// receives bytes. It never learns a path, and there is nothing remote to
    /// fall back to (ADR 045).
    pub fn read_asset(
        &self,
        asset: &AssetRef,
        offset: i64,
        length: i64,
    ) -> Result<Vec<u8>, AppError> {
        Ok(self.assets.read_at(asset, offset, length)?)
    }

    /// What one snapshot needs, and what of it is here.
    ///
    /// Derived from the snapshot's own content rows, so it cannot describe a
    /// different snapshot than the one it names (ADR 045).
    pub fn get_snapshot_assets(
        &self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<SnapshotAssetsView, AppError> {
        let required = self
            .store()
            .transact(|uow| uow.list_catalog_required_assets(snapshot_id))?;
        let mut assets = Vec::with_capacity(required.len());
        for asset in required {
            let state = self.assets.status(&asset).unwrap_or(AssetState::Missing);
            assets.push(AssetStatusView { asset, state });
        }
        // Usage is reported, never enforced: nothing is ever evicted to make
        // room for an import (ADR 045).
        let stored_bytes = self.assets.stored_bytes().unwrap_or(0);
        Ok(SnapshotAssetsView {
            snapshot_id,
            assets,
            stored_bytes,
        })
    }

    /// The gate in front of activation.
    ///
    /// Two passes, on purpose. The cheap one first, so a snapshot whose images
    /// have plainly not arrived is refused without hashing anything; then the
    /// real one, because "a file of the right length exists" is not the
    /// promise the snapshot made — the promise is a digest (ADR 045).
    ///
    /// A snapshot that references no asset never touches the store at all,
    /// which is what keeps every retail catalog exactly as it was.
    pub(crate) fn require_assets_ready(
        &self,
        uow: &mut dyn UnitOfWork,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<(), AppError> {
        let required = uow.list_catalog_required_assets(snapshot_id)?;
        if required.is_empty() {
            return Ok(());
        }

        let mut pending = 0_usize;
        let mut first = None;
        for asset in &required {
            let detail = match self.assets.status(asset) {
                Ok(state) if state.is_ready() => continue,
                Ok(state) => format!("`{}` is {}", asset.content_hash, state.as_str()),
                Err(error) => format!("`{}`: {error}", asset.content_hash),
            };
            pending += 1;
            first.get_or_insert(detail);
        }
        if let Some(detail) = first {
            return Err(AppError::CatalogAssetsNotReady {
                snapshot_id: snapshot_id.to_string(),
                pending,
                detail,
            });
        }

        for asset in &required {
            self.assets
                .verify(asset)
                .map_err(|error| AppError::CatalogAssetsNotReady {
                    snapshot_id: snapshot_id.to_string(),
                    pending: 1,
                    detail: format!("`{}`: {error}", asset.content_hash),
                })?;
        }
        tracing::info!(
            snapshot_id = %snapshot_id,
            assets = required.len(),
            "every asset this snapshot needs is verified locally"
        );
        Ok(())
    }
}
