//! Sync over the real IPC transport, and the runtime's own dispatcher.
//!
//! What only this layer can show: that the **production default is no sync at
//! all** and says so over the wire, that the daemon drains its own outbox once
//! a transport is configured without any client asking, and that the protocol
//! carries no way for a client to name a server. There is no URL, no host and
//! no credential in any operation or any payload (ADR 040).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::{Duration, Instant};

use common::*;
use edge_client::{EdgeClient, IpcEdgeClient};

const SKU: &str = "SKU-001";

/// The simulator, asked for explicitly. A test that talks to a fake server
/// says so out loud (ADR 040).
const SIMULATED_SYNC: [(&str, &str); 1] = [("PI_EDGE_SYNC", "simulator")];

/// Poll until the predicate holds, or give up. The daemon's dispatcher runs on
/// its own schedule, so a test waits for it rather than guessing.
fn wait_for(
    client: &IpcEdgeClient,
    label: &str,
    ready: impl Fn(&edge_protocol::SyncStatusV1) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let status = client.get_sync_status().unwrap();
        if ready(&status) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "timed out waiting for {label}: {:?}",
        client.get_sync_status()
    );
}

/// The production default, over the real transport.
#[test]
fn a_daemon_with_no_sync_configured_says_so_and_keeps_selling() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    // No PI_EDGE_SYNC.
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);

    let status = client.get_sync_status().unwrap();
    assert_eq!(status.driver, "unconfigured");
    assert_ne!(status.driver, "simulator", "absence selects nothing");
    assert_eq!(status.inbound_cursor, None);

    // Selling works, and the fact is kept.
    finalized_cash_sale(&client, 11_600);
    let status = client.get_sync_status().unwrap();
    assert_eq!(status.pending, 1);
    assert_eq!(status.sent, 0);

    // Asking for a cycle is answered plainly, and changes nothing.
    let cycle = client.run_sync_cycle(None).unwrap();
    assert_eq!(cycle.outcome, "NOT_CONFIGURED");
    assert_eq!(cycle.acknowledged, 0);
    assert_eq!(client.get_sync_status().unwrap().pending, 1);

    // Another sale, still offline. This is a normal register, not a broken
    // one.
    finalized_cash_sale(&client, 11_600);
    assert_eq!(client.get_sync_status().unwrap().pending, 2);
}

/// An unknown driver refuses to start, rather than syncing somewhere nobody
/// asked for.
#[test]
fn a_daemon_refuses_to_start_with_an_unknown_sync_driver() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let failure = DaemonProcess::start_failing(
        &database,
        &[("PI_EDGE_SYNC", "https://pi.commerce.example")],
    );
    assert!(
        failure.contains("PI_EDGE_SYNC"),
        "the daemon must name the bad setting: {failure}"
    );
}

/// The runtime drains its own outbox. No client asks it to.
#[test]
fn the_daemon_dispatches_its_facts_on_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_with_env(&database, None, true, &SIMULATED_SYNC);
    let client = client(&database, "edge-cli");
    assert_eq!(client.get_sync_status().unwrap().driver, "simulator");

    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);
    finalized_cash_sale(&client, 11_600);

    // Nobody called `run_sync_cycle`: the daemon's dispatcher did it.
    wait_for(&client, "the sale to be delivered", |status| {
        status.sent == 1
    });
    let status = client.get_sync_status().unwrap();
    assert_eq!(status.pending, 0);
    assert_eq!(status.sending, 0);
    assert_eq!(status.failed, 0);
}

/// A restart does not lose a fact, and does not deliver one twice.
#[test]
fn facts_written_while_offline_are_delivered_after_a_restart_with_sync() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    {
        // A week of trading with no server anywhere.
        let _daemon = DaemonProcess::start(&database);
        let client = client(&database, "edge-cli");
        open_register(&client);
        bootstrap_stock(&client, &[(SKU, 10)]);
        finalized_cash_sale(&client, 11_600);
        finalized_cash_sale(&client, 11_600);
        assert_eq!(client.get_sync_status().unwrap().pending, 2);
    }

    // The installation is connected for the first time.
    let _daemon = DaemonProcess::start_with_env(&database, None, true, &SIMULATED_SYNC);
    let client = client(&database, "edge-cli");
    wait_for(&client, "both sales to be delivered", |status| {
        status.sent == 2
    });
    let status = client.get_sync_status().unwrap();
    assert_eq!(status.pending, 0);
    // Nothing is purged: the delivered facts are still on record (ADR 041).
    assert_eq!(status.sent, 2);
    assert_eq!(client.get_pending_outbox(10).unwrap().pending_count, 0);
}

/// The protocol has no way to say "sync to this address".
#[test]
fn no_operation_can_name_a_server() {
    use edge_protocol::{LimitRequestV1, OperationV1};

    // The whole sync surface: a read and a trigger, neither carrying a
    // payload. Configuration lives in the composition root (ADR 040).
    for operation in [
        OperationV1::GetSyncStatus,
        OperationV1::RunSyncCycle,
        OperationV1::GetPendingOutbox(LimitRequestV1 { limit: 10 }),
    ] {
        let wire = serde_json::to_string(&operation).unwrap();
        for forbidden in ["http", "host", "port", "url", "token", "endpoint"] {
            assert!(
                !wire.to_lowercase().contains(forbidden),
                "`{forbidden}` must not appear in {wire}"
            );
        }
    }
}
