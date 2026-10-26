//! One-command offline bootstrap of the demo restaurant.
//!
//! ```text
//! edge-cli demo-seed --path demo
//!     provision the device
//!     import every file in demo/assets/   (hashed locally, verified by the core)
//!     import demo/menu.json               (its `file:` images resolved to hashes)
//!     activate the snapshot               (refused unless every image is here)
//!     open the business day and a kiosk session
//! ```
//!
//! Nothing here reaches the network, and nothing here is a mock: it drives the
//! same import, asset and session use cases the server eventually will.
//!
//! The dataset references its images by **stem** — `file:burger-classic` —
//! which this command resolves to whichever file in `assets/` carries that
//! stem, preferring a real photograph over a placeholder. Drop
//! `burger-classic.jpg` in beside `burger-classic.png` and the catalog picks
//! up the photograph: no hash, no extension and no filename to edit by hand
//! (ADR 046).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use edge_client::{EdgeClient, IpcEdgeClient};
use edge_protocol::{InitDeviceRequestV1, OpenRegisterSessionRequestV1, MAX_ASSET_CHUNK_BYTES};
use serde_json::{json, Value};

use crate::assets::import_asset;
use crate::catalog::import_catalog;
use crate::error::CliError;

/// The way of selling this demo is curated for.
pub const DEMO_CHANNEL: &str = "KIOSK";

#[derive(Debug)]
pub struct DemoOutcome {
    pub assets_imported: usize,
    pub assets_already_present: usize,
    pub snapshot_id: String,
    pub products: i64,
    pub activated: bool,
    pub session_opened: bool,
    /// Image stems the dataset asks for that nobody has supplied yet. Those
    /// rows render as tiles; dropping the file in turns them into cards.
    pub photographs_wanted: Vec<String>,
}

impl DemoOutcome {
    pub fn to_value(&self) -> Result<Value, CliError> {
        Ok(json!({
            "assets_imported": self.assets_imported,
            "assets_already_present": self.assets_already_present,
            "snapshot_id": self.snapshot_id,
            "products": self.products,
            "activated": self.activated,
            "session_opened": self.session_opened,
            "channel": DEMO_CHANNEL,
            "photographs_wanted": self.photographs_wanted,
        }))
    }
}

pub fn seed(client: &IpcEdgeClient, path: &Path) -> Result<DemoOutcome, CliError> {
    // 1. The device. Idempotent by nature: an existing profile is returned
    //    untouched, so re-seeding is safe.
    client.init_device(InitDeviceRequestV1 {
        tenant_id: "TENANT-NOVA".to_owned(),
        store_id: "STORE-NOVA-01".to_owned(),
        currency: "MXN".to_owned(),
        allow_negative_inventory: true,
    })?;

    // 2. The bytes, before the catalog that references them — otherwise
    //    activation would be refused, which is exactly what it should do
    //    (ADR 045).
    let asset_dir = path.join("assets");
    let mut assets_imported = 0;
    let mut assets_already_present = 0;
    let mut hashes = BTreeMap::new();
    for (stem, file) in best_per_stem(&asset_dir)? {
        let media_type = media_type_of(&file)?;
        let outcome = import_asset(client, &file, media_type, MAX_ASSET_CHUNK_BYTES)?;
        if outcome.already_present {
            assets_already_present += 1;
        } else {
            assets_imported += 1;
        }
        hashes.insert(stem, outcome.status.asset);
    }

    // 3. The catalog, with every `file:` reference resolved to what the core
    //    now holds.
    let source = path.join("menu.json");
    let raw = std::fs::read_to_string(&source)
        .map_err(|error| CliError::Input(format!("cannot read {}: {error}", source.display())))?;
    let mut fixture: Value = serde_json::from_str(&raw).map_err(|error| {
        CliError::Input(format!("{} is not valid JSON: {error}", source.display()))
    })?;
    let mut unsupplied = BTreeSet::new();
    resolve_assets(&mut fixture, &hashes, &mut unsupplied)?;

    // Not inside the demo directory: two seeds running at once would write
    // the same path and read each other's half-written file. The importer
    // only needs a path it can read, so give each run its own.
    let resolved = std::env::temp_dir().join(format!(
        "pi-edge-menu-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::write(
        &resolved,
        serde_json::to_vec_pretty(&fixture)
            .map_err(|error| CliError::Serialization(error.to_string()))?,
    )
    .map_err(|error| CliError::Input(format!("cannot write {}: {error}", resolved.display())))?;

    // 4. Import and activate through the real pipeline.
    let outcome = import_catalog(
        client,
        &resolved,
        crate::catalog::DEFAULT_CHUNK_BUDGET_BYTES,
        true,
    )?;
    let _ = std::fs::remove_file(&resolved);

    // 5. A day and a till, so the kiosk can sell the moment it opens.
    let session_opened = open_session(client)?;

    Ok(DemoOutcome {
        assets_imported,
        assets_already_present,
        snapshot_id: outcome.snapshot.snapshot.snapshot_id.clone(),
        products: outcome.snapshot.stored.products,
        activated: outcome.activated,
        session_opened,
        photographs_wanted: unsupplied.into_iter().collect(),
    })
}

/// Open the business day and the kiosk session, tolerating the ones that are
/// already open so re-seeding is safe.
fn open_session(client: &IpcEdgeClient) -> Result<bool, CliError> {
    if let Err(error) = client.open_business_day(None) {
        if !already(&error, "BUSINESS_DAY_ALREADY_OPEN") {
            return Err(error.into());
        }
    }
    match client.open_session(
        OpenRegisterSessionRequestV1 {
            register_id: "KIOSK-01".to_owned(),
            operator_id: "KIOSK".to_owned(),
            opening_cash_minor: 0,
        },
        None,
    ) {
        Ok(_) => Ok(true),
        Err(error) if already(&error, "REGISTER_SESSION_ALREADY_OPEN") => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn already(error: &edge_client::ClientError, code: &str) -> bool {
    format!("{error}").contains(code)
}

/// Rewrite every `{"content_hash": "file:<name>"}` with what the core stored.
///
/// A stem with no file in the asset directory is **not** an error. Every image
/// in this dataset is optional — a product, a category and an option all carry
/// `Option<AssetRef>` — so an unsupplied photograph becomes `null` and that
/// row renders as a tile instead of a card. This is what lets the dataset name
/// the pictures it *wants* before anyone has shot them: drop
/// `extra-cheese.jpg` into `demo/assets/` and the option turns visual with no
/// JSON to edit.
///
/// Nothing is dropped quietly: every unresolved stem is collected and printed
/// by the seed, so a typo looks like a typo rather than a missing picture.
fn resolve_assets(
    node: &mut Value,
    hashes: &BTreeMap<String, AssetRef>,
    missing: &mut BTreeSet<String>,
) -> Result<(), CliError> {
    match node {
        Value::Object(map) => {
            // Handled from the parent, because dropping an image means
            // nulling the field that holds it, not emptying the object.
            let mut unsupplied = Vec::new();
            for (key, value) in map.iter() {
                let stem = value
                    .as_object()
                    .and_then(|image| image.get("content_hash"))
                    .and_then(Value::as_str)
                    .and_then(|hash| hash.strip_prefix("file:"));
                if let Some(stem) = stem {
                    if !hashes.contains_key(stem) {
                        unsupplied.push((key.clone(), stem.to_owned()));
                    }
                }
            }
            for (key, stem) in unsupplied {
                missing.insert(stem);
                map.insert(key, Value::Null);
            }

            let named = match map.get("content_hash").and_then(Value::as_str) {
                Some(value) => value.strip_prefix("file:").map(str::to_owned),
                None => None,
            };
            if let Some(name) = named {
                let asset = hashes.get(&name).ok_or_else(|| {
                    CliError::Input(format!(
                        "the dataset references `{name}`, which is not in the asset directory"
                    ))
                })?;
                map.insert("content_hash".to_owned(), json!(asset.content_hash));
                map.insert("media_type".to_owned(), json!(asset.media_type));
                map.insert("byte_size".to_owned(), json!(asset.byte_size));
            }
            for value in map.values_mut() {
                resolve_assets(value, hashes, missing)?;
            }
        }
        Value::Array(items) => {
            for value in items {
                resolve_assets(value, hashes, missing)?;
            }
        }
        _ => {}
    }
    Ok(())
}

type AssetRef = edge_protocol::AssetRefV1;

/// One file per stem, a photograph winning over a placeholder.
///
/// The dataset names an image by stem, so `burger-classic.jpg` dropped in
/// beside `burger-classic.png` replaces it with no edit anywhere — and only
/// the winner is imported, so a superseded placeholder costs no disk. Files
/// that are not images this runtime holds (the folder's own notes, say) are
/// skipped rather than refused.
fn best_per_stem(dir: &Path) -> Result<Vec<(String, PathBuf)>, CliError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| CliError::Input(format!("cannot read {}: {error}", dir.display())))?;
    let mut best: BTreeMap<String, (u8, PathBuf)> = BTreeMap::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| CliError::Input(format!("cannot read an entry: {error}")))?;
        let path = entry.path();
        if !path.is_file() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let rank = photo_rank(&path);
        let stem = match path.file_stem() {
            Some(stem) if rank > 0 => stem.to_string_lossy().into_owned(),
            _ => continue,
        };
        match best.get(&stem) {
            Some((existing, _)) if *existing >= rank => {}
            _ => {
                best.insert(stem, (rank, path));
            }
        }
    }
    Ok(best
        .into_iter()
        .map(|(stem, (_, path))| (stem, path))
        .collect())
}

/// How much a file looks like the real photograph rather than the stand-in.
/// Zero means "not an image this runtime holds".
fn photo_rank(path: &Path) -> u8 {
    match path
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg" | "webp") => 3,
        Some("png") => 2,
        Some("gif" | "mp4") => 1,
        _ => 0,
    }
}

/// What the bytes are, from the extension. The core verifies the claim against
/// the bytes themselves, so a wrong guess is refused rather than stored.
fn media_type_of(path: &Path) -> Result<&'static str, CliError> {
    match path
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .as_deref()
    {
        Some("png") => Ok("image/png"),
        Some("jpg" | "jpeg") => Ok("image/jpeg"),
        Some("webp") => Ok("image/webp"),
        Some("gif") => Ok("image/gif"),
        Some("mp4") => Ok("video/mp4"),
        other => Err(CliError::Input(format!(
            "`{}` has no media type this runtime holds ({other:?})",
            path.display()
        ))),
    }
}
