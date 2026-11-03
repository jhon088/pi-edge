//! One-command offline bootstrap of the demo restaurant.
//!
//! ```text
//! edge-cli demo-seed --path demo [--overrides overrides.json]
//!     provision the device
//!     import every file in demo/assets/   (hashed locally, verified by the core)
//!     import demo/menu.json               (its `file:` images resolved to hashes,
//!                                          its prices overridden if asked)
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
    /// Products whose price came from the overrides file rather than the
    /// dataset.
    pub repriced: Vec<String>,
    /// Overrides naming a product this dataset does not have.
    ///
    /// Reported rather than dropped: an override for `T-LATE` is a typo, and
    /// a typo that silently does nothing is a price somebody thinks they
    /// changed.
    pub repriced_unknown: Vec<String>,
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
            "repriced": self.repriced,
            "repriced_unknown": self.repriced_unknown,
        }))
    }
}

pub fn seed(
    client: &IpcEdgeClient,
    path: &Path,
    overrides: Option<&Path>,
) -> Result<DemoOutcome, CliError> {
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
    // Every vertical's assets, from its own directory. A stem defined in two
    // of them would be a mistake in the datasets, not something to resolve
    // here, so `best_per_stem` sees one flat list and the later definition
    // wins exactly as it would within one directory.
    let mut sources = best_per_stem(&path.join("assets"))?;
    for vertical in verticals(path)? {
        let assets = vertical.join("assets");
        if assets.is_dir() {
            sources.extend(best_per_stem(&assets)?);
        }
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
    // Every other vertical that ships. One snapshot carries all of them,
    // because a store may have exactly one active catalog — the lanes are
    // kept apart by channel availability, not by swapping catalogs underneath
    // a running till (ADR 044, ADR 047).
    for vertical in verticals(path)? {
        let menu = vertical.join("menu.json");
        if menu.is_file() {
            merge_vertical(&mut fixture, &menu)?;
        }
    }
    // Prices an operator changed on this kiosk, applied before the import so
    // the catalog in force is the one that carries them.
    let prices = price_overrides(overrides)?;
    let repriced = apply_price_overrides(&mut fixture, &prices);
    let unknown: Vec<String> = prices
        .keys()
        .filter(|product_ref| !repriced.contains(*product_ref))
        .cloned()
        .collect();

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
        repriced: repriced.into_iter().collect(),
        repriced_unknown: unknown,
    })
}

/// Prices an operator changed on the kiosk itself.
///
/// Read from a file the host writes and applied to the dataset **before** it
/// is imported, so the number that comes out the other side is the one the
/// catalog in force charges. There is deliberately no second place a price
/// lives: a sale reads the active snapshot and nothing else, so an override
/// that did not travel through this function would be cosmetic (ADR 050).
///
/// Shape, keyed by `product_ref`:
///
/// ```json
/// { "T-LATTE": { "price_minor": 5900 } }
/// ```
///
/// Only `price_minor` is read here. The flag that says a product needs
/// authorization lives in the same file and is **not** catalog data — it is a
/// showroom gate the host owns, and the core never sees it.
fn price_overrides(path: Option<&Path>) -> Result<BTreeMap<String, i64>, CliError> {
    let Some(path) = path else {
        return Ok(BTreeMap::new());
    };
    // A missing file is "nothing overridden", not a failure: a kiosk nobody
    // has edited prices on has no such file, and the seed still has to run.
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Ok(BTreeMap::new());
    };
    let parsed: BTreeMap<String, Value> = serde_json::from_str(&raw).map_err(|error| {
        CliError::Input(format!("{} is not valid JSON: {error}", path.display()))
    })?;
    let mut prices = BTreeMap::new();
    for (product_ref, entry) in parsed {
        let Some(minor) = entry.get("price_minor") else {
            continue;
        };
        let minor = minor.as_i64().ok_or_else(|| {
            CliError::Input(format!(
                "`{product_ref}` has a price_minor that is not a whole number of cents"
            ))
        })?;
        if minor < 0 {
            return Err(CliError::Input(format!(
                "`{product_ref}` is priced at {minor}; a price cannot be negative"
            )));
        }
        prices.insert(product_ref, minor);
    }
    Ok(prices)
}

/// Rewrite every price of every overridden product, in place.
///
/// Every price book, because the demo has one and a kiosk that edits a price
/// means "this costs that now" rather than "in this one book". Answers which
/// products were actually found, so the seed can report an override naming a
/// product the dataset does not have instead of dropping it quietly.
fn apply_price_overrides(fixture: &mut Value, prices: &BTreeMap<String, i64>) -> BTreeSet<String> {
    let mut applied = BTreeSet::new();
    if prices.is_empty() {
        return applied;
    }
    let Some(products) = fixture.get_mut("products").and_then(Value::as_array_mut) else {
        return applied;
    };
    for product in products.iter_mut() {
        let Some(product_ref) = product
            .get("product_ref")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        let Some(minor) = prices.get(&product_ref) else {
            continue;
        };
        if let Some(entries) = product.get_mut("prices").and_then(Value::as_array_mut) {
            for entry in entries.iter_mut() {
                if let Some(map) = entry.as_object_mut() {
                    map.insert("amount_minor".to_owned(), json!(minor));
                }
            }
            applied.insert(product_ref);
        }
    }
    applied
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

/// The vertical directories beside the base dataset, in a stable order.
///
/// Any subdirectory of `demo/` is a vertical: drop one in with a `menu.json`
/// and an `assets/` folder and it is part of the installation. Sorted, so two
/// runs produce the same snapshot and therefore the same content hash.
fn verticals(path: &Path) -> Result<Vec<PathBuf>, CliError> {
    let entries = std::fs::read_dir(path)
        .map_err(|error| CliError::Input(format!("cannot read {}: {error}", path.display())))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| CliError::Input(format!("cannot read an entry: {error}")))?;
        let candidate = entry.path();
        // `assets/` is the base dataset's own, not a vertical.
        if candidate.is_dir() && candidate.file_name().is_some_and(|name| name != "assets") {
            found.push(candidate);
        }
    }
    found.sort();
    Ok(found)
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

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    fn dataset() -> Value {
        json!({
            "products": [
                {
                    "product_ref": "T-LATTE",
                    "sku": "T-LATTE",
                    "prices": [
                        { "price_book": "BASE", "amount_minor": 5_500, "price_mode": "TAX_EXCLUSIVE" },
                    ],
                },
                {
                    "product_ref": "T-CAFE-DARK",
                    "sku": "T-CAFE-DARK",
                    "prices": [
                        { "price_book": "BASE", "amount_minor": 3_900, "price_mode": "TAX_EXCLUSIVE" },
                    ],
                },
            ],
        })
    }

    fn price_of(fixture: &Value, product_ref: &str) -> i64 {
        fixture["products"]
            .as_array()
            .unwrap()
            .iter()
            .find(|product| product["product_ref"] == product_ref)
            .unwrap()["prices"][0]["amount_minor"]
            .as_i64()
            .unwrap()
    }

    /// An override reaches the dataset the importer reads, which is the whole
    /// point: the price that comes out is the one the catalog charges.
    #[test]
    fn an_override_replaces_the_price_the_dataset_declared() {
        let mut fixture = dataset();
        let applied = apply_price_overrides(
            &mut fixture,
            &BTreeMap::from([("T-LATTE".to_owned(), 5_900)]),
        );
        assert_eq!(applied, BTreeSet::from(["T-LATTE".to_owned()]));
        assert_eq!(price_of(&fixture, "T-LATTE"), 5_900);
        // And nothing else moved.
        assert_eq!(price_of(&fixture, "T-CAFE-DARK"), 3_900);
    }

    /// Nothing to override leaves the dataset exactly as it shipped.
    #[test]
    fn no_overrides_changes_nothing_at_all() {
        let mut fixture = dataset();
        let before = fixture.clone();
        assert!(apply_price_overrides(&mut fixture, &BTreeMap::new()).is_empty());
        assert_eq!(fixture, before);
    }

    /// An override naming a product the dataset does not have is reported,
    /// not dropped: a typo is a price somebody thinks they changed.
    #[test]
    fn an_override_for_a_product_that_does_not_exist_is_reported() {
        let mut fixture = dataset();
        let applied = apply_price_overrides(
            &mut fixture,
            &BTreeMap::from([("T-LATE".to_owned(), 5_900), ("T-LATTE".to_owned(), 6_100)]),
        );
        assert_eq!(applied, BTreeSet::from(["T-LATTE".to_owned()]));
        assert!(
            !applied.contains("T-LATE"),
            "a typo was applied to something"
        );
        assert_eq!(price_of(&fixture, "T-LATTE"), 6_100);
    }

    /// Zero is a price. It is what a showroom uses to give something away,
    /// and it must not be mistaken for "no override".
    #[test]
    fn a_price_of_zero_is_an_override_like_any_other() {
        let mut fixture = dataset();
        let applied =
            apply_price_overrides(&mut fixture, &BTreeMap::from([("T-LATTE".to_owned(), 0)]));
        assert_eq!(applied, BTreeSet::from(["T-LATTE".to_owned()]));
        assert_eq!(price_of(&fixture, "T-LATTE"), 0);
    }

    /// The file the host writes, read back.
    #[test]
    fn the_overrides_file_is_read_for_prices_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overrides.json");
        std::fs::write(
            &path,
            r#"{
                 "T-LATTE": { "price_minor": 5900 },
                 "R-ARRACHERA": { "price_minor": 28900, "authorization_required": true },
                 "R-CLOROX": { "authorization_required": true }
               }"#,
        )
        .unwrap();
        let prices = price_overrides(Some(&path)).unwrap();
        assert_eq!(prices.get("T-LATTE"), Some(&5_900));
        assert_eq!(prices.get("R-ARRACHERA"), Some(&28_900));
        // The authorization flag is not catalog data and the core never sees
        // it: a product with only that flag contributes no price.
        assert!(!prices.contains_key("R-CLOROX"));
        assert_eq!(prices.len(), 2);
    }

    /// A kiosk nobody has edited prices on has no such file, and the seed
    /// still has to run.
    #[test]
    fn a_missing_overrides_file_is_nothing_overridden_rather_than_a_failure() {
        assert!(price_overrides(None).unwrap().is_empty());
        assert!(price_overrides(Some(Path::new("/nowhere/overrides.json")))
            .unwrap()
            .is_empty());
    }

    /// What cannot be a price is refused rather than coerced.
    #[test]
    fn a_price_that_is_not_a_whole_number_of_cents_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        for bad in [
            r#"{ "T-LATTE": { "price_minor": "5900" } }"#,
            r#"{ "T-LATTE": { "price_minor": 59.5 } }"#,
            r#"{ "T-LATTE": { "price_minor": -100 } }"#,
        ] {
            let path = dir.path().join("overrides.json");
            std::fs::write(&path, bad).unwrap();
            assert!(
                price_overrides(Some(&path)).is_err(),
                "`{bad}` was accepted as a price"
            );
        }
        // And a file that is not JSON at all says so rather than being
        // treated as empty: a corrupt overrides file is an operator's edit
        // gone wrong, and seeding past it would charge the old prices.
        let path = dir.path().join("overrides.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(price_overrides(Some(&path)).is_err());
    }
}
