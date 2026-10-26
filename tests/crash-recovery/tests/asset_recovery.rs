//! The asset runtime across process deaths (ADR 045).
//!
//! The store has no table to keep in step with the filesystem, which is what
//! makes these windows converge instead of needing recovery: a file named by
//! its own digest either is there and verifies, or it is not there.
//!
//! What still needs proving is the *coupling* with activation — that a death
//! in the middle of putting a new catalog in force never leaves a snapshot
//! active without its images, and never takes the selling catalog down.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::testing::{CatalogFixture, FixtureProduct};
use edge_assets::AssetState;
use edge_catalog::{AssetRef, LocalizedText, ProductPresentation};
use edge_crash_recovery_tests::*;
use edge_domain::prelude::*;

fn illustrated(version: i64, image: AssetRef) -> CatalogFixture {
    CatalogFixture::new(version)
        .with(FixtureProduct::new("BURGER", "Hamburguesa", 12_000).barcode("7501000000100"))
        .with_presentation(ProductPresentation {
            product_ref: ProductRef::new("BURGER").unwrap(),
            category_ref: None,
            display_name: Some(LocalizedText::new("Hamburguesa").unwrap()),
            description: None,
            display_order: 0,
            image: Some(image),
            recommended_product_refs: Vec::new(),
        })
}

fn stage_and_commit(core: &TestCore, bytes: &[u8], asset: &AssetRef) {
    core.put_asset_chunk(PutAssetChunkCommand {
        asset: asset.clone(),
        offset: 0,
        bytes: bytes.to_vec(),
    })
    .unwrap();
    core.commit_asset(CommitAssetCommand {
        asset: asset.clone(),
    })
    .unwrap();
}

/// A) The process dies while bytes are still arriving. The content store is
/// untouched, the asset is never `READY`, and the next lifetime resumes.
#[test]
fn a_death_during_staging_leaves_the_content_store_intact() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let (good, good_ref) = png_asset(1, 400);
    let (partial, partial_ref) = png_asset(2, 400);

    {
        let core = core_at_with_assets(&db, 1);
        init_device(&core);
        stage_and_commit(&core, &good, &good_ref);
        // Half of the second asset arrives, then the process dies.
        core.put_asset_chunk(PutAssetChunkCommand {
            asset: partial_ref.clone(),
            offset: 0,
            bytes: partial[..150].to_vec(),
        })
        .unwrap();
    }

    let core = core_at_with_assets(&db, 1_000);
    assert_eq!(
        core.get_asset_status(good_ref.clone()).unwrap().state,
        AssetState::Ready,
        "the asset that was finished is still finished"
    );
    assert_eq!(
        core.get_asset_status(partial_ref.clone()).unwrap().state,
        AssetState::Staged {
            bytes_received: 150
        },
        "and the one that was not is resumable, not lost and not ready"
    );
    assert_eq!(
        core.assets().stored_bytes().unwrap(),
        good.len() as i64,
        "nothing unverified reached the content store"
    );

    // Resuming finishes it.
    core.put_asset_chunk(PutAssetChunkCommand {
        asset: partial_ref.clone(),
        offset: 150,
        bytes: partial[150..].to_vec(),
    })
    .unwrap();
    core.commit_asset(CommitAssetCommand {
        asset: partial_ref.clone(),
    })
    .unwrap();
    assert_eq!(
        core.get_asset_status(partial_ref).unwrap().state,
        AssetState::Ready
    );
}

/// B) and C) The process dies around the promotion. Committing again is the
/// whole of the recovery, because the file is the only state there is.
#[test]
fn a_death_around_promotion_converges_on_the_next_commit() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let (bytes, asset) = png_asset(3, 500);

    // B) verified and staged, dead before the rename: the commit that follows
    // the restart does the verification and the promotion.
    {
        let core = core_at_with_assets(&db, 1);
        init_device(&core);
        core.put_asset_chunk(PutAssetChunkCommand {
            asset: asset.clone(),
            offset: 0,
            bytes: bytes.clone(),
        })
        .unwrap();
    }
    {
        let core = core_at_with_assets(&db, 1_000);
        assert_eq!(
            core.commit_asset(CommitAssetCommand {
                asset: asset.clone()
            })
            .unwrap()
            .state,
            AssetState::Ready
        );
    }

    // C) promoted, dead before anything downstream could be recorded: the
    // retry finds the file and says so, writing nothing.
    let core = core_at_with_assets(&db, 2_000);
    let before = core.assets().stored_bytes().unwrap();
    assert_eq!(
        core.commit_asset(CommitAssetCommand {
            asset: asset.clone()
        })
        .unwrap()
        .state,
        AssetState::Ready
    );
    assert_eq!(core.assets().stored_bytes().unwrap(), before, "no new copy");
}

/// D) A snapshot waiting for its images, across a restart: it keeps waiting,
/// and the catalog that was selling keeps selling.
#[test]
fn a_snapshot_waiting_for_assets_still_waits_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let (present, present_ref) = png_asset(4, 300);
    let (absent, absent_ref) = png_asset(5, 300);

    let waiting = {
        let core = core_at_with_assets(&db, 1);
        init_device(&core);
        stage_and_commit(&core, &present, &present_ref);
        illustrated(1, present_ref.clone()).activate(&core).unwrap();

        let view = illustrated(2, absent_ref.clone()).import(&core).unwrap();
        let snapshot_id = view.snapshot.id();
        assert_eq!(
            core.activate_catalog_snapshot(ActivateCatalogSnapshotCommand {
                request_id: None,
                snapshot_id,
            })
            .unwrap_err()
            .kind(),
            "CATALOG_ASSET_NOT_READY"
        );
        snapshot_id
    };

    let core = core_at_with_assets(&db, 1_000);
    let status = core.get_catalog_status().unwrap();
    assert_eq!(
        status.active.as_ref().unwrap().snapshot.version(),
        1,
        "the previous snapshot is still the one that prices sales"
    );
    assert_eq!(
        core.get_snapshot_assets(waiting).unwrap().pending(),
        1,
        "and the new one is still one image short"
    );
    assert_eq!(
        core.activate_catalog_snapshot(ActivateCatalogSnapshotCommand {
            request_id: None,
            snapshot_id: waiting,
        })
        .unwrap_err()
        .kind(),
        "CATALOG_ASSET_NOT_READY"
    );

    // The bytes arrive in this lifetime, and only then does it go in force.
    stage_and_commit(&core, &absent, &absent_ref);
    core.activate_catalog_snapshot(ActivateCatalogSnapshotCommand {
        request_id: None,
        snapshot_id: waiting,
    })
    .unwrap();
    assert_eq!(
        core.get_catalog_status()
            .unwrap()
            .active
            .unwrap()
            .snapshot
            .version(),
        2
    );
}

/// The window the assets introduce into activation: verification passed, and
/// the process died before the `COMMIT` that would have put the snapshot in
/// force. Nothing is half-activated, the old catalog is untouched, and the
/// retry succeeds without re-sending a byte.
#[test]
fn a_death_between_verifying_assets_and_committing_the_activation_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let (first, first_ref) = png_asset(6, 300);
    let (second, second_ref) = png_asset(7, 300);

    let core = core_at_with_assets(&db, 1);
    init_device(&core);
    stage_and_commit(&core, &first, &first_ref);
    stage_and_commit(&core, &second, &second_ref);
    illustrated(1, first_ref).activate(&core).unwrap();

    let next = illustrated(2, second_ref.clone())
        .import(&core)
        .unwrap()
        .snapshot
        .id();

    // Power loss after every write of the activation, before the COMMIT.
    core.store().arm_commit_failure();
    assert!(core
        .activate_catalog_snapshot(ActivateCatalogSnapshotCommand {
            request_id: None,
            snapshot_id: next,
        })
        .is_err());

    // A brand new core reads SQLite: version 1 is still the catalog, and
    // version 2 is still PENDING rather than half in force.
    let recovered = core_at_with_assets(&db, 1_000);
    let status = recovered.get_catalog_status().unwrap();
    assert_eq!(status.active.as_ref().unwrap().snapshot.version(), 1);
    assert_eq!(status.pending.len(), 1);
    assert_eq!(status.pending[0].snapshot.id(), next);
    // The assets never needed recovering, so the retry transfers nothing.
    assert_eq!(
        recovered.get_asset_status(second_ref).unwrap().state,
        AssetState::Ready
    );
    recovered
        .activate_catalog_snapshot(ActivateCatalogSnapshotCommand {
            request_id: None,
            snapshot_id: next,
        })
        .unwrap();
    assert_eq!(
        recovered
            .get_catalog_status()
            .unwrap()
            .active
            .unwrap()
            .snapshot
            .version(),
        2
    );
}

/// E) A corrupt asset is never `READY`, whatever happens to the process, and
/// the activation gate is what notices.
#[test]
fn a_corrupt_asset_never_becomes_ready() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let (bytes, asset) = png_asset(8, 300);
    let (other, _) = png_asset(9, 300);

    {
        let core = core_at_with_assets(&db, 1);
        init_device(&core);
        // The declared reference belongs to one image; these are another's
        // bytes.
        core.put_asset_chunk(PutAssetChunkCommand {
            asset: asset.clone(),
            offset: 0,
            bytes: other,
        })
        .unwrap();
        assert_eq!(
            core.commit_asset(CommitAssetCommand {
                asset: asset.clone()
            })
            .unwrap_err()
            .kind(),
            "ASSET_DIGEST_MISMATCH"
        );
    }

    let core = core_at_with_assets(&db, 1_000);
    assert_eq!(
        core.get_asset_status(asset.clone()).unwrap().state,
        AssetState::Missing,
        "across the restart it is still absent, never ready"
    );
    let snapshot_id = illustrated(1, asset.clone())
        .import(&core)
        .unwrap()
        .snapshot
        .id();
    assert_eq!(
        core.activate_catalog_snapshot(ActivateCatalogSnapshotCommand {
            request_id: None,
            snapshot_id,
        })
        .unwrap_err()
        .kind(),
        "CATALOG_ASSET_NOT_READY"
    );

    // And the honest bytes under the same reference are accepted.
    stage_and_commit(&core, &bytes, &asset);
    core.activate_catalog_snapshot(ActivateCatalogSnapshotCommand {
        request_id: None,
        snapshot_id,
    })
    .unwrap();
}
