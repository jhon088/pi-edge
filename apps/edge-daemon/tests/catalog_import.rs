//! Chunked catalog import over the real IPC transport.
//!
//! Covers the whole transfer contract: many chunks, replays, conflicts, gaps,
//! malformed records, a bad hash, a daemon restart mid-import, and atomic
//! activation.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::process::Command;

use common::{client, product, CatalogSpec, DaemonProcess};
use edge_client::EdgeClient;
use edge_protocol::{
    CatalogProductRecordV1, CommitCatalogImportRequestV1, InitDeviceRequestV1, ProductSelectorV1,
};

fn provision(client: &edge_client::IpcEdgeClient) {
    client
        .init_device(InitDeviceRequestV1 {
            tenant_id: "TENANT-DEMO".to_owned(),
            store_id: "STORE-DEMO".to_owned(),
            currency: "MXN".to_owned(),
        })
        .unwrap();
}

/// `count` products, each with its own barcode.
fn many_products(count: usize) -> Vec<CatalogProductRecordV1> {
    (0..count)
        .map(|index| {
            product(
                &format!("75010{index:08}"),
                &format!("SKU-{index:05}"),
                1_000 + index as i64,
                "IVA16",
            )
        })
        .collect()
}

#[test]
fn imports_a_large_snapshot_in_many_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    provision(&client);

    let spec = CatalogSpec::with_products(1, many_products(250));
    let opened = client
        .begin_catalog_import(spec.begin_request(), None)
        .unwrap();
    let snapshot_id = opened.snapshot.snapshot_id.clone();
    assert_eq!(opened.snapshot.status, "IMPORTING");
    assert_eq!(opened.progress.next_chunk_index, 0);

    // 25 chunks of 10 records: the record count per chunk is the client's
    // business, not the protocol's.
    let chunks: Vec<&[CatalogProductRecordV1]> = spec.products.chunks(10).collect();
    for (index, records) in chunks.iter().enumerate() {
        let status = client
            .put_catalog_chunk(spec.chunk(&snapshot_id, index as i64, records))
            .unwrap();
        assert_eq!(status.progress.received_chunks.len(), index + 1);
        assert_eq!(status.progress.next_chunk_index, index as i64 + 1);
        // The daemon persists as it goes: stored counts grow per chunk.
        assert_eq!(status.progress.stored.products, (index as i64 + 1) * 10);
    }

    let view = client
        .commit_catalog_import(
            CommitCatalogImportRequestV1 {
                snapshot_id: snapshot_id.clone(),
                chunk_count: chunks.len() as i64,
            },
            None,
        )
        .unwrap();
    assert_eq!(view.snapshot.status, "PENDING");
    assert_eq!(view.stored.products, 250);
    assert_eq!(view.stored.barcodes, 250);
    // The core stored the hash it computed from its own rows.
    assert_eq!(
        view.snapshot.content_hash.as_deref(),
        Some(view.snapshot.declared_content_hash.as_str())
    );

    let activated = client
        .activate_catalog_snapshot(&snapshot_id, None)
        .unwrap();
    assert_eq!(activated.snapshot.status, "ACTIVE");

    // And a product from the middle of the catalog resolves.
    let lookup = client
        .lookup_product(ProductSelectorV1::Sku("SKU-00123".to_owned()))
        .unwrap();
    let price = lookup.price.as_ref().expect("a price must resolve");
    assert_eq!(price.unit_price.minor, 1_123);
    assert_eq!(price.price_book_code, "BASE");
}

#[test]
fn resending_an_identical_chunk_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    provision(&client);

    let spec = CatalogSpec::with_products(1, many_products(20));
    let opened = client
        .begin_catalog_import(spec.begin_request(), None)
        .unwrap();
    let snapshot_id = opened.snapshot.snapshot_id.clone();

    let chunk = spec.chunk(&snapshot_id, 0, &spec.products[..10]);
    let first = client.put_catalog_chunk(chunk.clone()).unwrap();
    let replay = client.put_catalog_chunk(chunk).unwrap();

    assert_eq!(first.progress.stored.products, 10);
    assert_eq!(replay.progress.stored.products, 10);
    assert_eq!(replay.progress.received_chunks, vec![0]);
}

#[test]
fn resending_a_chunk_index_with_different_content_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    provision(&client);

    let spec = CatalogSpec::with_products(1, many_products(20));
    let opened = client
        .begin_catalog_import(spec.begin_request(), None)
        .unwrap();
    let snapshot_id = opened.snapshot.snapshot_id.clone();

    client
        .put_catalog_chunk(spec.chunk(&snapshot_id, 0, &spec.products[..10]))
        .unwrap();
    let error = client
        .put_catalog_chunk(spec.chunk(&snapshot_id, 0, &spec.products[10..]))
        .expect_err("a chunk index cannot change content");
    assert_eq!(error.code(), "CATALOG_CHUNK_CONFLICT");

    // The original chunk is intact.
    let status = client.catalog_import_status(&snapshot_id).unwrap();
    assert_eq!(status.progress.stored.products, 10);
}

#[test]
fn a_missing_chunk_blocks_the_commit() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    provision(&client);

    let spec = CatalogSpec::with_products(1, many_products(30));
    let opened = client
        .begin_catalog_import(spec.begin_request(), None)
        .unwrap();
    let snapshot_id = opened.snapshot.snapshot_id.clone();

    // Send chunks 0 and 2, skipping 1.
    client
        .put_catalog_chunk(spec.chunk(&snapshot_id, 0, &spec.products[..10]))
        .unwrap();
    client
        .put_catalog_chunk(spec.chunk(&snapshot_id, 2, &spec.products[20..]))
        .unwrap();

    let error = client
        .commit_catalog_import(
            CommitCatalogImportRequestV1 {
                snapshot_id: snapshot_id.clone(),
                chunk_count: 3,
            },
            None,
        )
        .expect_err("a gap must block the commit");
    assert_eq!(error.code(), "CATALOG_IMPORT_INCOMPLETE");

    // Still importing, so the client can send the missing chunk and retry.
    let status = client.catalog_import_status(&snapshot_id).unwrap();
    assert_eq!(status.snapshot.status, "IMPORTING");
    client
        .put_catalog_chunk(spec.chunk(&snapshot_id, 1, &spec.products[10..20]))
        .unwrap();
    let view = client
        .commit_catalog_import(
            CommitCatalogImportRequestV1 {
                snapshot_id,
                chunk_count: 3,
            },
            None,
        )
        .unwrap();
    assert_eq!(view.snapshot.status, "PENDING");
    assert_eq!(view.stored.products, 30);
}

#[test]
fn a_short_chunk_count_blocks_the_commit() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    provision(&client);

    let spec = CatalogSpec::with_products(1, many_products(20));
    let opened = client
        .begin_catalog_import(spec.begin_request(), None)
        .unwrap();
    let snapshot_id = opened.snapshot.snapshot_id.clone();
    client
        .put_catalog_chunk(spec.chunk(&snapshot_id, 0, &spec.products[..10]))
        .unwrap();

    // The client believes it sent two chunks; only one arrived.
    let error = client
        .commit_catalog_import(
            CommitCatalogImportRequestV1 {
                snapshot_id,
                chunk_count: 2,
            },
            None,
        )
        .expect_err("a lost tail must be detected");
    assert_eq!(error.code(), "CATALOG_IMPORT_INCOMPLETE");
}

#[test]
fn a_malformed_record_is_refused_without_disturbing_earlier_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    provision(&client);

    let spec = CatalogSpec::with_products(1, many_products(20));
    let opened = client
        .begin_catalog_import(spec.begin_request(), None)
        .unwrap();
    let snapshot_id = opened.snapshot.snapshot_id.clone();
    client
        .put_catalog_chunk(spec.chunk(&snapshot_id, 0, &spec.products[..10]))
        .unwrap();

    // A negative price is not a price.
    let mut broken = spec.products[10..].to_vec();
    broken[3].prices[0].amount_minor = -1;
    let mut request = spec.chunk(&snapshot_id, 1, &spec.products[10..]);
    request.products = broken.clone();
    let error = client
        .put_catalog_chunk(request)
        .expect_err("a malformed record must be refused");
    assert_eq!(error.code(), "CATALOG_INVALID_RECORD");

    // A product referencing a profile the snapshot never declared.
    let mut unknown_profile = spec.chunk(&snapshot_id, 1, &spec.products[10..]);
    unknown_profile.products[0].tax_profile = "IVA-NOPE".to_owned();
    assert_eq!(
        client
            .put_catalog_chunk(unknown_profile)
            .expect_err("unknown profile")
            .code(),
        "CATALOG_UNKNOWN_TAX_PROFILE"
    );

    // Two records in the same chunk claiming one barcode.
    let mut duplicate = spec.chunk(&snapshot_id, 1, &spec.products[10..]);
    duplicate.products[1].barcodes = duplicate.products[0].barcodes.clone();
    assert_eq!(
        client
            .put_catalog_chunk(duplicate)
            .expect_err("duplicate barcode")
            .code(),
        "CATALOG_DUPLICATE_BARCODE"
    );

    // Nothing from the rejected chunks survived, and chunk 0 is untouched.
    let status = client.catalog_import_status(&snapshot_id).unwrap();
    assert_eq!(status.progress.received_chunks, vec![0]);
    assert_eq!(status.progress.stored.products, 10);
    assert_eq!(status.progress.stored.barcodes, 10);

    // The good version of chunk 1 still goes through.
    client
        .put_catalog_chunk(spec.chunk(&snapshot_id, 1, &spec.products[10..]))
        .unwrap();
    let view = client
        .commit_catalog_import(
            CommitCatalogImportRequestV1 {
                snapshot_id,
                chunk_count: 2,
            },
            None,
        )
        .unwrap();
    assert_eq!(view.stored.products, 20);
}

#[test]
fn a_content_hash_that_does_not_match_the_stored_rows_blocks_the_commit() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    provision(&client);

    let spec = CatalogSpec::with_products(1, many_products(10));
    let mut request = spec.begin_request();
    request.content_hash =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_owned();
    let opened = client.begin_catalog_import(request, None).unwrap();
    let snapshot_id = opened.snapshot.snapshot_id.clone();
    client
        .put_catalog_chunk(spec.chunk(&snapshot_id, 0, &spec.products))
        .unwrap();

    let error = client
        .commit_catalog_import(
            CommitCatalogImportRequestV1 {
                snapshot_id: snapshot_id.clone(),
                chunk_count: 1,
            },
            None,
        )
        .expect_err("the core must not accept a hash it cannot reproduce");
    assert_eq!(error.code(), "CATALOG_HASH_MISMATCH");

    // Never partially usable: still importing, nothing active.
    let status = client.catalog_import_status(&snapshot_id).unwrap();
    assert_eq!(status.snapshot.status, "IMPORTING");
    assert!(client.catalog_status().unwrap().active.is_none());
}

#[test]
fn an_import_resumes_after_the_daemon_is_killed_halfway() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let spec = CatalogSpec::with_products(1, many_products(40));

    let snapshot_id = {
        let mut daemon = DaemonProcess::start(&database);
        let client = client(&database, "edge-cli");
        provision(&client);
        let opened = client
            .begin_catalog_import(spec.begin_request(), None)
            .unwrap();
        let snapshot_id = opened.snapshot.snapshot_id.clone();
        for index in 0..2 {
            client
                .put_catalog_chunk(spec.chunk(
                    &snapshot_id,
                    index,
                    &spec.products[index as usize * 10..(index as usize + 1) * 10],
                ))
                .unwrap();
        }
        daemon.kill_hard();
        snapshot_id
    };

    // New runtime, same installation: the import is still open and knows how
    // far it got.
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    let resumed = client
        .begin_catalog_import(spec.begin_request(), None)
        .unwrap();
    assert_eq!(resumed.snapshot.snapshot_id, snapshot_id);
    assert_eq!(resumed.snapshot.status, "IMPORTING");
    assert_eq!(resumed.progress.received_chunks, vec![0, 1]);
    assert_eq!(resumed.progress.next_chunk_index, 2);
    assert_eq!(resumed.progress.stored.products, 20);

    for index in resumed.progress.next_chunk_index..4 {
        client
            .put_catalog_chunk(spec.chunk(
                &snapshot_id,
                index,
                &spec.products[index as usize * 10..(index as usize + 1) * 10],
            ))
            .unwrap();
    }
    let view = client
        .commit_catalog_import(
            CommitCatalogImportRequestV1 {
                snapshot_id: snapshot_id.clone(),
                chunk_count: 4,
            },
            None,
        )
        .unwrap();
    assert_eq!(view.snapshot.status, "PENDING");
    assert_eq!(view.stored.products, 40);
    assert_eq!(
        view.snapshot.content_hash.as_deref(),
        Some(spec.declared_hash().as_str())
    );
}

#[test]
fn activation_is_atomic_and_leaves_exactly_one_active_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    provision(&client);

    CatalogSpec::demo(1).import(&client, true);
    let second = CatalogSpec::with_products(2, many_products(5)).import(&client, true);

    let status = client.catalog_status().unwrap();
    assert_eq!(
        status.active.as_ref().unwrap().snapshot.snapshot_id,
        second.snapshot.snapshot_id
    );
    assert_eq!(status.active.as_ref().unwrap().snapshot.version, 2);
    assert!(status.pending.is_empty());
    assert!(status.importing.is_empty());

    // Re-activating what is already in force changes nothing.
    let again = client
        .activate_catalog_snapshot(&second.snapshot.snapshot_id, None)
        .unwrap();
    assert_eq!(again.snapshot.status, "ACTIVE");
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

#[test]
fn an_aborted_import_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    provision(&client);

    let spec = CatalogSpec::with_products(1, many_products(10));
    let opened = client
        .begin_catalog_import(spec.begin_request(), None)
        .unwrap();
    let snapshot_id = opened.snapshot.snapshot_id.clone();
    client
        .put_catalog_chunk(spec.chunk(&snapshot_id, 0, &spec.products))
        .unwrap();

    client.abort_catalog_import(&snapshot_id, None).unwrap();
    assert_eq!(
        client
            .catalog_import_status(&snapshot_id)
            .expect_err("the import is gone")
            .code(),
        "CATALOG_SNAPSHOT_NOT_FOUND"
    );
    let status = client.catalog_status().unwrap();
    assert!(status.importing.is_empty());
    assert!(status.active.is_none());

    // The version is free again.
    let reopened = client
        .begin_catalog_import(spec.begin_request(), None)
        .unwrap();
    assert_ne!(reopened.snapshot.snapshot_id, snapshot_id);
    assert!(reopened.progress.received_chunks.is_empty());
}

#[test]
fn the_cli_importer_streams_a_catalog_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    provision(&client);

    let fixture_path = dir.path().join("catalog.json");
    let spec = CatalogSpec::with_products(1, many_products(120));
    spec.write_fixture(&fixture_path);

    // A tiny byte budget forces many chunks through the real CLI.
    let output = Command::new(common::cli_binary())
        .arg("--db")
        .arg(&database)
        .arg("--format")
        .arg("json")
        .arg("import-catalog")
        .arg(&fixture_path)
        .arg("--activate")
        .arg("--chunk-bytes")
        .arg("2048")
        .env("PI_EDGE_LOG", "warn")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "import failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("the CLI prints JSON");
    assert_eq!(report["activated"], true);
    assert_eq!(report["snapshot"]["snapshot"]["status"], "ACTIVE");
    assert_eq!(report["snapshot"]["stored"]["products"], 120);
    assert!(
        report["chunks_sent"].as_i64().unwrap() > 1,
        "a 2 KiB budget must produce several chunks: {report}"
    );

    // Re-running the same import is a no-op, not a second snapshot.
    let again = Command::new(common::cli_binary())
        .arg("--db")
        .arg(&database)
        .arg("--format")
        .arg("json")
        .arg("import-catalog")
        .arg(&fixture_path)
        .env("PI_EDGE_LOG", "warn")
        .output()
        .unwrap();
    assert!(again.status.success());
    let report: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(report["chunks_sent"], 0);
    assert_eq!(report["snapshot"]["snapshot"]["status"], "ACTIVE");

    let status = client.catalog_status().unwrap();
    assert_eq!(status.active.as_ref().unwrap().snapshot.version, 1);
    assert!(status.pending.is_empty());
}
