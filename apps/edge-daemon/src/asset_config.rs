//! Where this runtime keeps asset bytes, and how large one may be.
//!
//! Read here and nowhere else. A use case, the protocol and every client are
//! incapable of naming a directory: they name a content hash, and the store
//! decides where that lives (ADR 045).

use std::path::Path;
use std::sync::Arc;

use edge_assets::{FileSystemAssetStore, LocalAssetStore, UnconfiguredAssetStore};

/// Ceiling on one asset, in bytes.
pub const MAX_BYTES_VAR: &str = "PI_EDGE_MAX_ASSET_BYTES";

/// Open the store for this installation.
///
/// A store that cannot be created does **not** stop the runtime: a till with
/// no asset directory can still sell, and the only consequence is that a
/// snapshot which references an image cannot be activated — which is exactly
/// what should happen when its bytes have nowhere to live (ADR 045). It is
/// never a silent fallback to a temporary directory or to the network.
pub fn open(root: &Path) -> Result<Arc<dyn LocalAssetStore>, String> {
    let limit = match std::env::var(MAX_BYTES_VAR) {
        Ok(raw) => {
            let trimmed = raw.trim();
            let parsed: i64 = trimmed
                .parse()
                .map_err(|_| format!("{MAX_BYTES_VAR}=`{trimmed}` is not a byte count"))?;
            if parsed <= 0 {
                return Err(format!("{MAX_BYTES_VAR} must be positive"));
            }
            parsed
        }
        Err(std::env::VarError::NotPresent) => edge_assets::DEFAULT_MAX_ASSET_BYTES,
        Err(error) => return Err(format!("{MAX_BYTES_VAR} is unreadable: {error}")),
    };

    match FileSystemAssetStore::with_limit(root, limit) {
        Ok(store) => {
            tracing::info!(
                max_asset_bytes = store.max_asset_bytes(),
                "local asset store ready"
            );
            Ok(Arc::new(store))
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "no local asset store: a snapshot that references an image cannot be activated"
            );
            Ok(Arc::new(UnconfiguredAssetStore))
        }
    }
}
