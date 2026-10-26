//! The local asset runtime, end to end against real SQLite and a real
//! directory (ADR 045).
//!
//! The rule everything here is arranged around:
//!
//! > **A snapshot never becomes ACTIVE while an image it references is
//! > missing, the wrong size, or does not hash to what it claims.**
//!
//! So the tests are mostly about what *fails*: a catalog with a photo nobody
//! sent stays `PENDING`, and the catalog already selling keeps selling.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use edge_application::commands::*;
use edge_application::ports::EdgeStore;
use edge_application::testing::{CatalogFixture, FixtureProduct};
use edge_application::AppError;
use edge_assets::AssetState;
use edge_catalog::{
    AssetRef, CatalogError, Category, Locale, LocalizedText, ProductPresentation, SelectionGroup,
    SelectionOption, SelectionOwner,
};
use edge_domain::prelude::*;
use edge_integration_tests::*;

// ------------------------------------------------------------------ builders

fn text(value: &str) -> LocalizedText {
    LocalizedText::new(value).unwrap()
}

fn product_ref(value: &str) -> ProductRef {
    ProductRef::new(value).unwrap()
}

fn category_ref(value: &str) -> CategoryRef {
    CategoryRef::new(value).unwrap()
}

/// A snapshot whose burger has a photo.
fn menu_with_product_image(version: i64, image: AssetRef) -> CatalogFixture {
    CatalogFixture::new(version)
        .with(FixtureProduct::new("BURGER", "Hamburguesa", 12_000).barcode("7501000000100"))
        .with_presentation(ProductPresentation {
            product_ref: product_ref("BURGER"),
            category_ref: None,
            display_name: Some(text("Hamburguesa clásica")),
            description: None,
            display_order: 0,
            image: Some(image),
            recommended_product_refs: Vec::new(),
        })
}

/// A snapshot whose *category* has a photo and whose product has none.
fn menu_with_category_image(version: i64, image: AssetRef) -> CatalogFixture {
    CatalogFixture::new(version)
        .with(FixtureProduct::new("BURGER", "Hamburguesa", 12_000).barcode("7501000000100"))
        .with_category(Category::import(
            category_ref("C-BURGERS"),
            text("Hamburguesas"),
            0,
            Some(image),
        ))
        .with_presentation(ProductPresentation {
            product_ref: product_ref("BURGER"),
            category_ref: Some(category_ref("C-BURGERS")),
            display_name: None,
            description: None,
            display_order: 0,
            image: None,
            recommended_product_refs: Vec::new(),
        })
}

fn snapshot_of(core: &TestCore, fixture: &CatalogFixture) -> CatalogSnapshotId {
    fixture.import(core).unwrap().snapshot.id()
}

fn activate(core: &TestCore, snapshot_id: CatalogSnapshotId) -> Result<(), AppError> {
    core.activate_catalog_snapshot(ActivateCatalogSnapshotCommand {
        request_id: None,
        snapshot_id,
    })
    .map(|_| ())
}

fn active_version(core: &TestCore) -> Option<i64> {
    core.get_catalog_status()
        .unwrap()
        .active
        .map(|view| view.snapshot.version())
}

// -------------------------------------------------- activation is coordinated

#[test]
fn a_product_image_nobody_sent_blocks_activation() {
    let (core, _dir) = core_with_assets();
    init_device(&core);
    let bytes = png_bytes(1);
    let image = asset_ref_for(&bytes);

    // The snapshot imports and commits: the *catalog* is complete and its
    // content hash verifies. Only the bytes are missing.
    let snapshot_id = snapshot_of(&core, &menu_with_product_image(1, image.clone()));
    let error = activate(&core, snapshot_id).expect_err("refused");
    assert_eq!(error.kind(), "CATALOG_ASSET_NOT_READY");
    assert!(
        matches!(&error, AppError::CatalogAssetsNotReady { pending: 1, detail, .. }
            if detail.contains(&image.content_hash)),
        "the error names the asset that is missing: {error}"
    );
    assert_eq!(active_version(&core), None, "and nothing is in force");

    // Send the bytes; the same call now succeeds.
    store_asset(&core, &bytes, 16);
    activate(&core, snapshot_id).unwrap();
    assert_eq!(active_version(&core), Some(1));
}

#[test]
fn a_category_image_nobody_sent_blocks_activation() {
    let (core, _dir) = core_with_assets();
    init_device(&core);
    let bytes = png_bytes(2);
    let snapshot_id = snapshot_of(&core, &menu_with_category_image(1, asset_ref_for(&bytes)));

    assert_eq!(
        activate(&core, snapshot_id).unwrap_err().kind(),
        "CATALOG_ASSET_NOT_READY",
        "a withheld section's artwork counts as much as a product's"
    );
    store_asset(&core, &bytes, 64);
    activate(&core, snapshot_id).unwrap();
    assert_eq!(active_version(&core), Some(1));
}

#[test]
fn an_option_image_nobody_sent_blocks_activation() {
    let (core, _dir) = core_with_assets();
    init_device(&core);
    let bytes = png_bytes(3);
    let fixture = CatalogFixture::new(1)
        .with(FixtureProduct::new("BURGER", "Hamburguesa", 12_000))
        .with(FixtureProduct::new("BACON", "Tocino", 1_500))
        .with_selection(
            SelectionGroup::import(
                SelectionGroupId::new("G-EXTRAS").unwrap(),
                SelectionOwner::Product {
                    product_ref: product_ref("BURGER"),
                },
                text("Extras"),
                0,
                2,
                0,
            )
            .unwrap(),
            vec![SelectionOption::import(
                SelectionOptionId::new("O-BACON").unwrap(),
                SelectionGroupId::new("G-EXTRAS").unwrap(),
                text("Tocino"),
                Some(product_ref("BACON")),
                1,
                0,
                0,
                Some(asset_ref_for(&bytes)),
            )
            .unwrap()],
        );
    let snapshot_id = snapshot_of(&core, &fixture);

    assert_eq!(
        activate(&core, snapshot_id).unwrap_err().kind(),
        "CATALOG_ASSET_NOT_READY",
        "every AssetRef the snapshot carries is required, including an option's"
    );
    store_asset(&core, &bytes, 1_000);
    activate(&core, snapshot_id).unwrap();
}

#[test]
fn the_catalog_already_selling_survives_a_refused_activation() {
    let (core, _dir) = core_with_assets();
    init_device(&core);

    // Version 1 has no images and goes in force.
    CatalogFixture::demo(1).activate(&core).unwrap();
    assert_eq!(active_version(&core), Some(1));

    // Version 2 wants a photo nobody sent.
    let bytes = png_bytes(4);
    let next = snapshot_of(&core, &menu_with_product_image(2, asset_ref_for(&bytes)));
    assert_eq!(
        activate(&core, next).unwrap_err().kind(),
        "CATALOG_ASSET_NOT_READY"
    );

    // Nothing was dismounted: the till keeps selling the catalog it had.
    assert_eq!(active_version(&core), Some(1));
    open_day_and_session(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    let sale = core.add_item(demo_item()).unwrap();
    assert_eq!(sale.state().lines[0].line_total.minor(), 11_600);
    assert_eq!(
        core.get_catalog_status()
            .unwrap()
            .pending
            .iter()
            .filter(|view| view.snapshot.id() == next)
            .count(),
        1,
        "the refused snapshot is still PENDING and still resumable"
    );
}

#[test]
fn a_catalog_whose_assets_are_all_ready_activates_atomically() {
    let (core, _dir) = core_with_assets();
    init_device(&core);

    let burger = png_bytes(5);
    let section = png_bytes(6);
    store_asset(&core, &burger, 32);
    store_asset(&core, &section, 32);

    let fixture = CatalogFixture::new(1)
        .with(FixtureProduct::new("BURGER", "Hamburguesa", 12_000).barcode("7501000000100"))
        .with_category(Category::import(
            category_ref("C-BURGERS"),
            text("Hamburguesas"),
            0,
            Some(asset_ref_for(&section)),
        ))
        .with_presentation(ProductPresentation {
            product_ref: product_ref("BURGER"),
            category_ref: Some(category_ref("C-BURGERS")),
            display_name: Some(text("Hamburguesa clásica")),
            description: None,
            display_order: 0,
            image: Some(asset_ref_for(&burger)),
            recommended_product_refs: Vec::new(),
        });
    let snapshot_id = snapshot_of(&core, &fixture);

    let manifest = core.get_snapshot_assets(snapshot_id).unwrap();
    assert_eq!(manifest.assets.len(), 2);
    assert!(manifest.is_ready());
    assert_eq!(manifest.pending(), 0);

    activate(&core, snapshot_id).unwrap();
    assert_eq!(active_version(&core), Some(1));
}

#[test]
fn a_product_with_no_asset_reference_blocks_nothing() {
    let (core, _dir) = core_with_assets();
    init_device(&core);

    // Presentation, a category, a translated name — and no image anywhere.
    // Absence of an `AssetRef` is "this product has no photo", never "a photo
    // is missing"; the two are not the same thing and are not confused here.
    let fixture = CatalogFixture::new(1)
        .with(FixtureProduct::new("BURGER", "Hamburguesa", 12_000).barcode("7501000000100"))
        .with_category(Category::import(
            category_ref("C-BURGERS"),
            text("Hamburguesas"),
            0,
            None,
        ))
        .with_presentation(ProductPresentation {
            product_ref: product_ref("BURGER"),
            category_ref: Some(category_ref("C-BURGERS")),
            display_name: Some(
                LocalizedText::with_translations(
                    "Hamburguesa",
                    BTreeMap::from([(Locale::new("en").unwrap(), "Burger".to_owned())]),
                )
                .unwrap(),
            ),
            description: None,
            display_order: 0,
            image: None,
            recommended_product_refs: Vec::new(),
        });
    let snapshot_id = snapshot_of(&core, &fixture);

    assert!(core
        .get_snapshot_assets(snapshot_id)
        .unwrap()
        .assets
        .is_empty());
    activate(&core, snapshot_id).unwrap();
    assert_eq!(active_version(&core), Some(1));
}

#[test]
fn a_retail_snapshot_never_touches_the_asset_runtime() {
    // No asset store configured at all — the honest default. A retail
    // catalog references nothing, so it imports and activates exactly as it
    // did before any of this existed.
    let core = in_memory_core();
    init_device(&core);
    let snapshot_id = CatalogFixture::demo(1).activate(&core).unwrap();

    assert!(core
        .get_snapshot_assets(snapshot_id)
        .unwrap()
        .assets
        .is_empty());
    assert_eq!(active_version(&core), Some(1));

    open_day_and_session(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    let sale = core.add_item(demo_item()).unwrap();
    assert_eq!(sale.state().lines[0].unit_price.minor(), 10_000);
    assert_eq!(sale.state().lines[0].line_total.minor(), 11_600);
}

#[test]
fn an_installation_with_no_asset_store_refuses_rather_than_improvises() {
    // The snapshot needs a photo; this till has nowhere to keep one. It must
    // not activate, and it must not quietly reach for a URL either.
    let core = in_memory_core();
    init_device(&core);
    let image = asset_ref_for(&png_bytes(7));
    let snapshot_id = snapshot_of(&core, &menu_with_product_image(1, image.clone()));

    assert_eq!(
        activate(&core, snapshot_id).unwrap_err().kind(),
        "CATALOG_ASSET_NOT_READY"
    );
    assert_eq!(active_version(&core), None);
    // And the write path says plainly that there is nowhere to write.
    assert_eq!(
        core.commit_asset(CommitAssetCommand { asset: image })
            .unwrap_err()
            .kind(),
        "ASSET_STORE_NOT_CONFIGURED"
    );
}

// ------------------------------------------------------------------- offline

#[test]
fn an_activated_snapshot_reads_its_images_with_no_network_of_any_kind() {
    let (core, _dir) = core_with_assets();
    init_device(&core);
    let burger = png_bytes(8);
    let section = png_bytes(9);
    store_asset(&core, &burger, 7);
    store_asset(&core, &section, 7);

    let fixture = CatalogFixture::new(1)
        .with(FixtureProduct::new("BURGER", "Hamburguesa", 12_000).barcode("7501000000100"))
        .with_category(Category::import(
            category_ref("C-BURGERS"),
            text("Hamburguesas"),
            0,
            Some(asset_ref_for(&section)),
        ))
        .with_presentation(ProductPresentation {
            product_ref: product_ref("BURGER"),
            category_ref: Some(category_ref("C-BURGERS")),
            display_name: Some(
                LocalizedText::with_translations(
                    "Hamburguesa clásica",
                    BTreeMap::from([(Locale::new("en").unwrap(), "Classic burger".to_owned())]),
                )
                .unwrap(),
            ),
            description: None,
            display_order: 0,
            image: Some(asset_ref_for(&burger)),
            recommended_product_refs: Vec::new(),
        });
    let snapshot_id = fixture.activate(&core).unwrap();

    // Everything a kiosk screen needs, from local storage only. There is no
    // transport configured on this core at all, so there is nothing that
    // could have fetched any of it.
    let presentation = core
        .store()
        .transact(|uow| uow.list_catalog_presentation(snapshot_id))
        .unwrap();
    let shown = &presentation[0];
    let display = shown.display_name.as_ref().unwrap();
    assert_eq!(
        display.resolve(Some(&Locale::new("en").unwrap())),
        "Classic burger"
    );
    assert_eq!(read_asset(&core, shown.image.as_ref().unwrap()), burger);

    let categories = core
        .store()
        .transact(|uow| uow.list_catalog_categories(snapshot_id))
        .unwrap();
    assert_eq!(
        read_asset(&core, categories[0].image.as_ref().unwrap()),
        section
    );
}

// ----------------------------------------------------------- the import path

#[test]
fn the_same_asset_is_stored_once_however_many_products_show_it() {
    let (core, _dir) = core_with_assets();
    init_device(&core);
    let bytes = png_bytes(10);
    let image = store_asset(&core, &bytes, 9);
    let stored_once = core.assets().stored_bytes().unwrap();

    // Two products and a category, one photo.
    let fixture = CatalogFixture::new(1)
        .with(FixtureProduct::new("BURGER", "Hamburguesa", 12_000))
        .with(FixtureProduct::new("FRIES", "Papas", 3_000))
        .with_category(Category::import(
            category_ref("C-ALL"),
            text("Todo"),
            0,
            Some(image.clone()),
        ))
        .with_presentation(ProductPresentation {
            product_ref: product_ref("BURGER"),
            category_ref: Some(category_ref("C-ALL")),
            display_name: None,
            description: None,
            display_order: 0,
            image: Some(image.clone()),
            recommended_product_refs: Vec::new(),
        })
        .with_presentation(ProductPresentation {
            product_ref: product_ref("FRIES"),
            category_ref: Some(category_ref("C-ALL")),
            display_name: None,
            description: None,
            display_order: 1,
            image: Some(image.clone()),
            recommended_product_refs: Vec::new(),
        });
    let snapshot_id = fixture.activate(&core).unwrap();

    let manifest = core.get_snapshot_assets(snapshot_id).unwrap();
    assert_eq!(
        manifest.assets.len(),
        1,
        "three references, one asset: the manifest is deduplicated by hash"
    );
    assert_eq!(manifest.assets[0].asset, image);
    assert_eq!(
        core.assets().stored_bytes().unwrap(),
        stored_once,
        "and one copy on disk"
    );
}

#[test]
fn two_snapshots_share_the_bytes_they_both_reference() {
    let (core, _dir) = core_with_assets();
    init_device(&core);
    let bytes = png_bytes(11);
    let image = store_asset(&core, &bytes, 1_000);
    let once = core.assets().stored_bytes().unwrap();

    menu_with_product_image(1, image.clone())
        .activate(&core)
        .unwrap();
    // Version 2 shows the same photo and sends nothing.
    menu_with_product_image(2, image).activate(&core).unwrap();

    assert_eq!(active_version(&core), Some(2));
    assert_eq!(core.assets().stored_bytes().unwrap(), once);
}

#[test]
fn an_interrupted_transfer_resumes_from_what_the_core_already_has() {
    let (core, _dir) = core_with_assets();
    init_device(&core);
    let bytes = png_bytes(12);
    let asset = asset_ref_for(&bytes);

    // Half of it arrives, then the transport dies.
    core.put_asset_chunk(PutAssetChunkCommand {
        asset: asset.clone(),
        offset: 0,
        bytes: bytes[..30].to_vec(),
    })
    .unwrap();
    let status = core.get_asset_status(asset.clone()).unwrap();
    assert_eq!(status.state, AssetState::Staged { bytes_received: 30 });

    // A snapshot that needs it cannot be activated from half an image.
    let snapshot_id = snapshot_of(&core, &menu_with_product_image(1, asset.clone()));
    assert_eq!(
        activate(&core, snapshot_id).unwrap_err().kind(),
        "CATALOG_ASSET_NOT_READY"
    );

    // The rest arrives, starting from what the core said it had.
    core.put_asset_chunk(PutAssetChunkCommand {
        asset: asset.clone(),
        offset: status.state.bytes_received(),
        bytes: bytes[30..].to_vec(),
    })
    .unwrap();
    core.commit_asset(CommitAssetCommand {
        asset: asset.clone(),
    })
    .unwrap();
    activate(&core, snapshot_id).unwrap();
    assert_eq!(read_asset(&core, &asset), bytes);
}

#[test]
fn a_client_cannot_talk_its_bytes_past_the_hash() {
    let (core, _dir) = core_with_assets();
    init_device(&core);
    let honest = png_bytes(13);
    let other = png_bytes(14);

    // A reference that describes one image, carrying another's bytes.
    let lying = asset_ref_for(&honest);
    core.put_asset_chunk(PutAssetChunkCommand {
        asset: lying.clone(),
        offset: 0,
        bytes: other.clone(),
    })
    .unwrap();
    assert_eq!(
        core.commit_asset(CommitAssetCommand {
            asset: lying.clone()
        })
        .unwrap_err()
        .kind(),
        "ASSET_DIGEST_MISMATCH"
    );
    assert_eq!(
        core.get_asset_status(lying.clone()).unwrap().state,
        AssetState::Missing,
        "nothing was stored, and the next attempt starts clean"
    );

    // A snapshot referencing it still cannot be activated.
    let snapshot_id = snapshot_of(&core, &menu_with_product_image(1, lying.clone()));
    assert_eq!(
        activate(&core, snapshot_id).unwrap_err().kind(),
        "CATALOG_ASSET_NOT_READY"
    );

    // And the honest bytes under the same reference work.
    store_asset(&core, &honest, 11);
    activate(&core, snapshot_id).unwrap();
}

#[test]
fn content_corrupted_under_the_store_stops_the_next_activation() {
    let (core, dir) = core_with_assets();
    init_device(&core);
    let bytes = png_bytes(15);
    let image = store_asset(&core, &bytes, 1_000);
    menu_with_product_image(1, image.clone())
        .activate(&core)
        .unwrap();

    // Something outside PI.Edge rewrites the file, keeping its length so only
    // re-reading can tell.
    let digest = image.content_hash.strip_prefix("sha256:").unwrap();
    let path = dir
        .path()
        .join("assets")
        .join("sha256")
        .join(&digest[..2])
        .join(digest);
    let mut tampered = bytes.clone();
    tampered[20] ^= 0xff;
    std::fs::write(&path, &tampered).unwrap();

    // The cheap check still says present, so only the activation gate's
    // SHA-256 pass catches it — which is why the gate does one.
    assert_eq!(
        core.get_asset_status(image.clone()).unwrap().state,
        AssetState::Ready
    );
    let next = snapshot_of(&core, &menu_with_product_image(2, image));
    let error = activate(&core, next).unwrap_err();
    assert_eq!(error.kind(), "CATALOG_ASSET_NOT_READY");
    assert!(
        format!("{error}").contains("hashes to"),
        "and it says the bytes are not the declared ones: {error}"
    );
    assert_eq!(active_version(&core), Some(1), "version 1 keeps selling");
}

#[test]
fn one_hash_cannot_be_two_assets() {
    let (core, _dir) = core_with_assets();
    init_device(&core);
    let bytes = png_bytes(16);
    let honest = asset_ref_for(&bytes);
    // The same bytes declared with a different length: the snapshot now has
    // two answers to "how big is this?", so its manifest is ambiguous.
    let contradiction = AssetRef::import(
        honest.content_hash.clone(),
        "image/png",
        honest.byte_size + 1,
    )
    .unwrap();

    let fixture = menu_with_product_image(1, honest).with_category(Category::import(
        category_ref("C-ALL"),
        text("Todo"),
        0,
        Some(contradiction),
    ));
    let error = fixture.import(&core).unwrap_err();
    assert!(
        matches!(
            error,
            AppError::Catalog(CatalogError::ConflictingAssetDeclaration { .. })
        ),
        "refused at commit, before anything could be activated: {error}"
    );
    assert_eq!(active_version(&core), None);
}

#[test]
fn an_asset_this_store_will_not_hold_is_refused_at_the_door() {
    let (core, _dir) = core_with_assets();
    init_device(&core);
    let bytes = png_bytes(17);
    let mut executable = asset_ref_for(&bytes);
    executable.media_type = "application/x-sh".to_owned();

    assert_eq!(
        core.put_asset_chunk(PutAssetChunkCommand {
            asset: executable.clone(),
            offset: 0,
            bytes: bytes.clone(),
        })
        .unwrap_err()
        .kind(),
        "ASSET_REJECTED"
    );
    assert_eq!(core.assets().stored_bytes().unwrap(), 0);
}
