//! The local asset runtime over the real IPC transport (ADR 045).
//!
//! What the protocol half has to prove: bytes cross the wire in chunks, the
//! core verifies them, a snapshot that references an image refuses to activate
//! until they are here, and nothing a client sends can talk its way past the
//! hash.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::process::Command;

use common::{client, product, CatalogSpec, DaemonProcess};
use edge_client::EdgeClient;
use edge_protocol::{
    AssetChunkRequestV1, AssetRefV1, CategoryV1, InitDeviceRequestV1, LocalizedTextV1,
    ProductPresentationV1, MAX_ASSET_CHUNK_BYTES,
};
use sha2::{Digest, Sha256};

fn provision(client: &edge_client::IpcEdgeClient) {
    client
        .init_device(InitDeviceRequestV1 {
            tenant_id: "TENANT-DEMO".to_owned(),
            store_id: "STORE-DEMO".to_owned(),
            currency: "MXN".to_owned(),
            allow_negative_inventory: true,
        })
        .unwrap();
}

/// A PNG big enough to need several chunks at a small budget.
fn png(tag: u8, length: usize) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend((0..length).map(|index| (index as u8).wrapping_mul(tag).wrapping_add(tag)));
    bytes
}

fn asset_ref(bytes: &[u8]) -> AssetRefV1 {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let mut hex = String::new();
    for byte in hasher.finalize() {
        hex.push_str(&format!("{byte:02x}"));
    }
    AssetRefV1 {
        content_hash: format!("sha256:{hex}"),
        media_type: "image/png".to_owned(),
        byte_size: bytes.len() as i64,
    }
}

fn wire_text(value: &str) -> LocalizedTextV1 {
    LocalizedTextV1 {
        default: value.to_owned(),
        translations: std::collections::BTreeMap::new(),
    }
}

/// A catalog whose single product has a photo.
fn illustrated(version: i64, image: AssetRefV1) -> CatalogSpec {
    CatalogSpec::with_products(
        version,
        vec![product("7501000000100", "BURGER", 12_000, "IVA16")],
    )
    .with_category(CategoryV1 {
        category_ref: "C-BURGERS".to_owned(),
        name: wire_text("Hamburguesas"),
        display_order: 0,
        image: None,
    })
    .with_presentation(ProductPresentationV1 {
        product_ref: "BURGER".to_owned(),
        category_ref: Some("C-BURGERS".to_owned()),
        display_name: Some(wire_text("Hamburguesa clásica")),
        description: None,
        display_order: 0,
        image: Some(image),
        recommended_product_refs: Vec::new(),
    })
}

/// Send an asset the way a transport would, in slices.
fn send(client: &edge_client::IpcEdgeClient, asset: &AssetRefV1, bytes: &[u8], chunk: usize) {
    let mut offset = 0_i64;
    for slice in bytes.chunks(chunk) {
        client
            .put_asset_chunk(AssetChunkRequestV1::new(asset.clone(), offset, slice))
            .unwrap();
        offset += slice.len() as i64;
    }
    let status = client.commit_asset(asset.clone()).unwrap();
    assert_eq!(status.state, "READY");
}

#[test]
fn an_asset_crosses_the_wire_in_chunks_and_unblocks_activation() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    provision(&client);

    let bytes = png(3, 5_000);
    let image = asset_ref(&bytes);
    let spec = illustrated(1, image.clone());

    // The catalog imports and commits with the bytes still absent: the
    // catalog is complete, the asset runtime is not.
    let view = spec.import(&client, false);
    assert_eq!(view.snapshot.status, "PENDING");
    let snapshot_id = view.snapshot.snapshot_id.clone();

    let manifest = client.snapshot_assets(&snapshot_id).unwrap();
    assert_eq!(manifest.assets.len(), 1);
    assert_eq!(manifest.assets[0].asset, image);
    assert_eq!(manifest.assets[0].state, "MISSING");
    assert_eq!(manifest.pending, 1);

    let refused = client
        .activate_catalog_snapshot(&snapshot_id, None)
        .expect_err("refused");
    assert!(
        format!("{refused}").contains("CATALOG_ASSET_NOT_READY"),
        "got {refused}"
    );

    // 512 bytes a chunk: ten frames for one image.
    send(&client, &image, &bytes, 512);
    let manifest = client.snapshot_assets(&snapshot_id).unwrap();
    assert_eq!(manifest.assets[0].state, "READY");
    assert_eq!(manifest.pending, 0);
    assert_eq!(manifest.stored_bytes, bytes.len() as i64);

    let activated = client
        .activate_catalog_snapshot(&snapshot_id, None)
        .unwrap();
    assert_eq!(activated.snapshot.status, "ACTIVE");
}

#[test]
fn a_transfer_that_died_halfway_resumes_over_the_same_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    provision(&client);

    let bytes = png(7, 3_000);
    let image = asset_ref(&bytes);

    client
        .put_asset_chunk(AssetChunkRequestV1::new(image.clone(), 0, &bytes[..1_000]))
        .unwrap();
    // …the client dies here. A new one asks what the core has.
    let resumed = common::client(&database, "tauri-pos");
    let status = resumed.asset_status(image.clone()).unwrap();
    assert_eq!(status.state, "STAGED");
    assert_eq!(status.bytes_received, 1_000);

    let offset = status.bytes_received as usize;
    resumed
        .put_asset_chunk(AssetChunkRequestV1::new(
            image.clone(),
            status.bytes_received,
            &bytes[offset..],
        ))
        .unwrap();
    assert_eq!(resumed.commit_asset(image.clone()).unwrap().state, "READY");

    // And a gap is refused rather than silently producing rubbish.
    let other = png(9, 2_000);
    let second = asset_ref(&other);
    client
        .put_asset_chunk(AssetChunkRequestV1::new(second.clone(), 0, &other[..100]))
        .unwrap();
    let gap = client
        .put_asset_chunk(AssetChunkRequestV1::new(second, 200, &other[200..]))
        .expect_err("refused");
    assert!(
        format!("{gap}").contains("ASSET_CHUNK_OUT_OF_ORDER"),
        "got {gap}"
    );
}

#[test]
fn the_core_hashes_what_it_received_and_not_what_it_was_told() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    provision(&client);

    let honest = png(11, 400);
    let other = png(12, 400);
    let lying = asset_ref(&honest);

    client
        .put_asset_chunk(AssetChunkRequestV1::new(lying.clone(), 0, &other))
        .unwrap();
    let refused = client.commit_asset(lying.clone()).expect_err("refused");
    assert!(
        format!("{refused}").contains("ASSET_DIGEST_MISMATCH"),
        "got {refused}"
    );
    assert_eq!(
        client.asset_status(lying.clone()).unwrap().state,
        "MISSING",
        "the bad bytes are gone, so a retry starts clean"
    );

    send(&client, &lying, &honest, MAX_ASSET_CHUNK_BYTES);
    assert_eq!(client.asset_status(lying).unwrap().state, "READY");
}

/// The CLI standing in for whatever eventually delivers bytes — a bundle, a
/// sync, a USB stick. It hashes a local file and streams it; it never says the
/// asset is valid.
#[test]
fn the_cli_imports_a_file_and_the_import_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    provision(&client);

    let bytes = png(13, 9_000);
    let file = dir.path().join("burger.png");
    std::fs::write(&file, &bytes).unwrap();

    let run = || {
        let output = Command::new(common::cli_binary())
            .arg("--db")
            .arg(&database)
            .arg("--format")
            .arg("json")
            .arg("import-asset")
            .arg(&file)
            .arg("--media-type")
            .arg("image/png")
            .arg("--chunk-bytes")
            .arg("1024")
            .env("PI_EDGE_LOG", "warn")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };

    let first = run();
    assert_eq!(first["asset"]["state"], "READY");
    assert_eq!(first["already_present"], false);
    assert_eq!(first["bytes_sent"], bytes.len() as i64);
    assert_eq!(first["chunks_sent"], 9);

    // Run it again: the core already has these bytes, so nothing is sent.
    let second = run();
    assert_eq!(second["asset"]["state"], "READY");
    assert_eq!(second["already_present"], true);
    assert_eq!(second["chunks_sent"], 0);
    assert_eq!(second["bytes_sent"], 0);
    assert_eq!(
        second["asset"]["asset"]["content_hash"], first["asset"]["asset"]["content_hash"],
        "the same file is the same asset"
    );

    // And now the catalog that references it activates.
    let image = asset_ref(&bytes);
    let view = illustrated(1, image).import(&client, true);
    assert_eq!(view.snapshot.status, "ACTIVE");

    // The manifest is reportable from the CLI too.
    let output = Command::new(common::cli_binary())
        .arg("--db")
        .arg(&database)
        .arg("--format")
        .arg("json")
        .arg("snapshot-assets")
        .arg("--snapshot")
        .arg(&view.snapshot.snapshot_id)
        .env("PI_EDGE_LOG", "warn")
        .output()
        .unwrap();
    assert!(output.status.success());
    let manifest: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(manifest["pending"], 0);
    assert_eq!(manifest["assets"][0]["state"], "READY");
}

#[test]
fn a_restarted_runtime_still_has_its_assets_and_still_waits_for_the_missing_one() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");

    let present = png(17, 600);
    let absent = png(19, 600);
    let snapshot_id;
    {
        let _daemon = DaemonProcess::start(&database);
        let client = client(&database, "edge-cli");
        provision(&client);
        send(&client, &asset_ref(&present), &present, 256);

        // Version 1 shows the photo that arrived and goes in force.
        illustrated(1, asset_ref(&present)).import(&client, true);
        // Version 2 shows one that did not.
        let view = illustrated(2, asset_ref(&absent)).import(&client, false);
        snapshot_id = view.snapshot.snapshot_id.clone();
        assert!(client
            .activate_catalog_snapshot(&snapshot_id, None)
            .is_err());
    }

    // A new runtime on the same installation.
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");

    let status = client.catalog_status().unwrap();
    assert_eq!(
        status.active.as_ref().unwrap().snapshot.version,
        1,
        "the catalog that was selling is still selling"
    );
    assert_eq!(
        client.asset_status(asset_ref(&present)).unwrap().state,
        "READY",
        "and the asset it needs survived the restart"
    );
    assert_eq!(
        client.snapshot_assets(&snapshot_id).unwrap().pending,
        1,
        "the waiting snapshot is still waiting"
    );
    assert!(client
        .activate_catalog_snapshot(&snapshot_id, None)
        .is_err());

    // The missing bytes arrive after the reboot, and version 2 goes in force.
    send(&client, &asset_ref(&absent), &absent, 256);
    let activated = client
        .activate_catalog_snapshot(&snapshot_id, None)
        .unwrap();
    assert_eq!(activated.snapshot.status, "ACTIVE");
    assert_eq!(
        client
            .catalog_status()
            .unwrap()
            .active
            .unwrap()
            .snapshot
            .version,
        2
    );
}
