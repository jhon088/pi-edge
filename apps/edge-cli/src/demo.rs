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

/// The installation this command is allowed to own.
///
/// `demo-seed` supersedes the catalog in force, and a catalog in force is
/// what prices a sale — so it must never do that to a real shop. There is no
/// metadata on a snapshot saying "a demo made this", and inventing one would
/// be a migration and a protocol field in service of a dev tool. What does
/// exist is the device profile: this command provisions exactly one
/// tenant/store pair, `init_device` returns an existing profile untouched
/// rather than overwriting it, and a real deployment is provisioned from the
/// server with real ids. So the profile is the guard.
///
/// **Limitation, stated rather than hidden:** a real catalog imported onto
/// *this* tenant and store would be indistinguishable from the demo's own and
/// would be superseded. Nothing but the demo should ever be `TENANT-NOVA`.
pub const DEMO_TENANT: &str = "TENANT-NOVA";
pub const DEMO_STORE: &str = "STORE-NOVA-01";

#[derive(Debug)]
pub struct DemoOutcome {
    pub assets_imported: usize,
    pub assets_already_present: usize,
    pub snapshot_id: String,
    pub products: i64,
    pub activated: bool,
    pub session_opened: bool,
    /// Catalog version actually in force after this run.
    pub catalog_version: i64,
    /// Whether this run moved the installation onto a different catalog than
    /// the one that was in force when it started — which is what an upgrade
    /// from an older demo looks like, and what a re-run must never be.
    pub catalog_upgraded: bool,
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
            "catalog_version": self.catalog_version,
            "catalog_upgraded": self.catalog_upgraded,
            "channel": DEMO_CHANNEL,
            "photographs_wanted": self.photographs_wanted,
        }))
    }
}

pub fn seed(client: &IpcEdgeClient, path: &Path) -> Result<DemoOutcome, CliError> {
    // 1. The device. Idempotent by nature: an existing profile is returned
    //    untouched, so re-seeding is safe — and so an installation that is
    //    already somebody's shop says so instead of being adopted.
    let device = client.init_device(InitDeviceRequestV1 {
        tenant_id: DEMO_TENANT.to_owned(),
        store_id: DEMO_STORE.to_owned(),
        currency: "MXN".to_owned(),
        allow_negative_inventory: true,
    })?;
    if device.tenant_id != DEMO_TENANT || device.store_id != DEMO_STORE {
        return Err(CliError::Input(format!(
            "this installation belongs to {}/{}, not to the demo ({DEMO_TENANT}/{DEMO_STORE}). \
             Refusing to supersede a catalog this command did not create; point --db at a \
             different file.",
            device.tenant_id, device.store_id
        )));
    }

    // 2. The bytes, before the catalog that references them — otherwise
    //    activation would be refused, which is exactly what it should do
    //    (ADR 045).
    // Both verticals' assets, from their own directories. A stem defined in
    // both would be a mistake in the datasets, not something to resolve here,
    // so `best_per_stem` sees one flat list and the later definition wins
    // exactly as it would within one directory.
    let mut sources = best_per_stem(&path.join("assets"))?;
    let retail_assets = path.join("retail").join("assets");
    if retail_assets.is_dir() {
        sources.extend(best_per_stem(&retail_assets)?);
    }
    let mut assets_imported = 0;
    let mut assets_already_present = 0;
    let mut hashes = BTreeMap::new();
    for (stem, file) in sources {
        let media_type = media_type_of(&file)?;
        let outcome = import_asset(client, &file, media_type, MAX_ASSET_CHUNK_BYTES)?;
        if outcome.already_present {
            assets_already_present += 1;
        } else {
            assets_imported += 1;
        }
        hashes.insert(stem, outcome.status.asset);
    }

    // What is in force before anything is imported, so "did this run move the
    // installation" is a fact rather than an inference. A version that differs
    // from the one in the file is not the same question: after an upgrade
    // every later run differs from the file and moves nothing.
    let in_force_before = client
        .catalog_status()?
        .active
        .map(|view| view.snapshot.version);

    // 3. The catalog, with every `file:` reference resolved to what the core
    //    now holds.
    let source = path.join("menu.json");
    let raw = std::fs::read_to_string(&source)
        .map_err(|error| CliError::Input(format!("cannot read {}: {error}", source.display())))?;
    let mut fixture: Value = serde_json::from_str(&raw).map_err(|error| {
        CliError::Input(format!("{} is not valid JSON: {error}", source.display()))
    })?;
    // The second vertical, if it ships. One snapshot carries both, because a
    // store may have exactly one active catalog — the two lanes are kept
    // apart by channel availability, not by swapping catalogs underneath a
    // running till (ADR 044).
    let retail = path.join("retail").join("menu.json");
    if retail.is_file() {
        merge_vertical(&mut fixture, &retail)?;
    }
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
        // The demo dataset ships with the application and changes between
        // releases — adding the retail vertical changed it — so the version in
        // the file is a default, not an assertion. The core is asked what it
        // already holds (ADR 047).
        crate::catalog::VersionPolicy::Reconcile,
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
        catalog_version: outcome.version,
        catalog_upgraded: in_force_before.is_some_and(|before| before != outcome.version),
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

/// Fold a second vertical's catalog into the one being imported.
///
/// Only the list-shaped sections are joined, and only by appending: a
/// vertical may add products, categories, channels and availability, and may
/// not redefine the tax profiles, price books or promotions the installation
/// already has. A reference that collides is left to fail at commit, where
/// the core checks the whole snapshot, rather than being silently merged
/// here into something nobody declared.
fn merge_vertical(base: &mut Value, source: &Path) -> Result<(), CliError> {
    let raw = std::fs::read_to_string(source)
        .map_err(|error| CliError::Input(format!("cannot read {}: {error}", source.display())))?;
    let extra: Value = serde_json::from_str(&raw).map_err(|error| {
        CliError::Input(format!("{} is not valid JSON: {error}", source.display()))
    })?;

    for section in [
        "channels",
        "categories",
        "products",
        "presentation",
        "selection_groups",
        "selection_options",
        "availability",
    ] {
        let Some(Value::Array(incoming)) = extra.get(section) else {
            continue;
        };
        let incoming = incoming.clone();
        match base.get_mut(section) {
            Some(Value::Array(existing)) => existing.extend(incoming),
            _ => {
                base.as_object_mut()
                    .ok_or_else(|| CliError::Input("the catalog is not an object".to_owned()))?
                    .insert(section.to_owned(), Value::Array(incoming));
            }
        }
    }
    Ok(())
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
