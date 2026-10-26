//! The milestone-2 acceptance test.
//!
//! Two independent clients (standing in for `edge-cli` and the Tauri UI) talk
//! to one real daemon process over the real IPC transport. The UI is killed and
//! restarted; the core is killed and restarted; the register survives both.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{client, demo_item, open_register, wait_until_connected, DaemonProcess, WatchProcess};
use edge_client::{EdgeClient, EventListener};
use edge_protocol::{CoreStatusV1, StartPaymentRequestV1};

const WAIT: Duration = Duration::from_secs(10);

#[test]
fn register_survives_ui_restarts_while_the_core_keeps_running() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let mut daemon = DaemonProcess::start(&database);

    // --- the CLI opens the day, the session and a sale ----------------------
    let cli = client(&database, "edge-cli");
    let info = cli.core_info().unwrap();
    assert_eq!(info.status, CoreStatusV1::Ready);
    assert_eq!(info.protocol_version, edge_protocol::PROTOCOL_VERSION);
    assert_eq!(info.database_schema_version, 14);
    assert_eq!(info.pid, daemon.pid);

    open_register(&cli);
    let sale = cli.start_transaction(None).unwrap();
    cli.add_item(demo_item(), None).unwrap();

    // --- the "UI" connects separately and sees the live sale ---------------
    let ui = client(&database, "tauri-pos");
    let state = ui.get_edge_state().unwrap();
    let active = state
        .active_transaction
        .as_ref()
        .expect("UI must see the sale");
    assert_eq!(active.transaction_id, sale.transaction_id);
    assert_eq!(active.lines.len(), 1);
    assert_eq!(active.lines[0].line_total.formatted, "116.00");

    // --- a real UI process subscribes and is pushed a change made by the CLI
    let mut watcher = WatchProcess::start(&database, 1);

    let totalized = cli.totalize(None).unwrap();
    assert_eq!(totalized.totals.grand_total.formatted, "116.00");
    let event = watcher.next_event();
    assert_eq!(event["event"], "EDGE_STATE_CHANGED");
    assert_eq!(event["payload"]["reason"], "TOTALIZE");
    assert!(event["payload"]["revision"].as_u64().unwrap() > 0);
    assert_eq!(event["protocol_version"], 1);

    // --- kill the UI completely: the subscriber process and its client -----
    watcher.kill_hard();
    assert!(!watcher.is_alive());
    drop(ui);
    assert!(daemon.is_alive(), "the core must outlive its UI");

    // --- the CLI carries on against the same runtime ----------------------
    let paid = cli
        .start_payment(
            StartPaymentRequestV1 {
                tender_type: "CASH".to_owned(),
                amount_minor: 20_000,
            },
            None,
        )
        .unwrap();
    assert_eq!(paid.transaction.status, "COMMITTED");
    // 200.00 handed over against a 116.00 sale: 116.00 collected, 84.00 back.
    assert_eq!(paid.payment.committed_amount.formatted, "116.00");
    assert_eq!(paid.payment.change_due.formatted, "84.00");
    assert_eq!(paid.transaction.change_due.formatted, "84.00");
    assert!(paid.settlement.can_finalize);

    // --- restart the UI: it rebuilds everything from the daemon ------------
    let ui = client(&database, "tauri-pos");
    let rebuilt = ui.get_edge_state().unwrap();
    let active = rebuilt.active_transaction.as_ref().unwrap();
    assert_eq!(active.transaction_id, sale.transaction_id);
    assert_eq!(active.status, "COMMITTED");
    // The sale collected 116.00; 200.00 is what the customer handed over.
    assert_eq!(active.paid_total.formatted, "116.00");
    assert_eq!(active.tendered_total.formatted, "200.00");
    assert_eq!(active.change_due.formatted, "84.00");
    assert!(active.basket_locked);
    assert!(!active.requires_payment_resolution);
    assert_eq!(rebuilt.business_day.as_ref().unwrap().status, "OPEN");
    assert_eq!(rebuilt.session.as_ref().unwrap().register_id, "REG-001");
    assert_eq!(rebuilt.pending_outbox, 0);

    // --- finish the sale from the UI client -------------------------------
    let finalized = ui.finalize_transaction(None).unwrap();
    assert_eq!(finalized.status, "FINALIZED");
    assert!(ui.get_active_transaction().unwrap().is_none());

    // journal and outbox, observed through the protocol
    let journal: Vec<String> = ui
        .get_journal(100)
        .unwrap()
        .into_iter()
        .rev()
        .map(|event| event.event_type)
        .collect();
    assert_eq!(
        journal,
        vec![
            "CATALOG_SNAPSHOT_IMPORTED",
            "CATALOG_SNAPSHOT_ACTIVATED",
            "BUSINESS_DAY_OPENED",
            "SESSION_OPENED",
            "TRANSACTION_STARTED",
            "ITEM_ADDED",
            "TRANSACTION_TOTALIZED",
            "PAYMENT_STARTED",
            "TRANSACTION_FINALIZED",
        ]
    );
    let outbox = ui.get_pending_outbox(10).unwrap();
    assert_eq!(outbox.pending_count, 1);
    assert_eq!(outbox.events[0].event_type, "SALE_FINALIZED");
    assert_eq!(outbox.events[0].status, "PENDING");

    // --- close the register and the day -----------------------------------
    // A session now closes blind: ask what is needed, then declare the count.
    // The requirements payload must not reveal what the drawer should hold.
    let requirements = ui.request_session_close(None).unwrap();
    assert!(requirements.blockers.is_empty());
    assert!(requirements.requires_declaration);
    // Structural check, not a visual one: the payload has no amount-bearing
    // key and no value equal to the expected figure (ADR 022). Ids are
    // excluded from the scan — a UUID can contain any digits by chance.
    let rendered = serde_json::to_value(&requirements).unwrap();
    let object = rendered.as_object().expect("an object");
    for key in object.keys() {
        assert!(
            !["expected_cash", "expected", "amount", "total", "movements"].contains(&key.as_str()),
            "the pre-close payload must not carry `{key}`"
        );
    }
    for (key, value) in object {
        if key == "session_id" {
            continue;
        }
        let rendered = value.to_string();
        for forbidden in ["111600", "1116.00", "100000", "1000.00"] {
            assert!(
                !rendered.contains(forbidden),
                "`{key}` must not reveal the expected cash: {rendered}"
            );
        }
    }
    // 1000.00 float + 116.00 collected in cash = 1116.00 expected.
    let closed = ui
        .declare_closing_cash(
            edge_protocol::DeclareClosingCashRequestV1 {
                counted_minor: 111_600,
            },
            None,
        )
        .unwrap();
    assert_eq!(closed.closure_kind, "DECLARED");
    assert_eq!(closed.expected_at_close.formatted, "1116.00");
    assert_eq!(closed.variance.as_ref().unwrap().minor, 0);
    assert_eq!(closed.variance_label, "BALANCED");
    assert_eq!(ui.close_business_day(None).unwrap().day.status, "CLOSED");
    let closed = cli.get_edge_state().unwrap();
    assert!(closed.business_day.is_none());
    assert!(closed.session.is_none());
    assert_eq!(closed.pending_outbox, 3);

    // --- graceful shutdown -------------------------------------------------
    #[cfg(unix)]
    assert_eq!(daemon.stop_gracefully(), 0, "SIGTERM must exit cleanly");
}

#[test]
fn ui_degrades_safely_when_the_core_dies_and_recovers_when_it_returns() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let mut daemon = DaemonProcess::start(&database);

    let ui = Arc::new(client(&database, "tauri-pos"));
    open_register(&ui);
    let sale = ui.start_transaction(None).unwrap();
    ui.add_item(demo_item(), None).unwrap();

    // The UI's event listener reconnects on its own, so count what it sees.
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    let listener = EventListener::spawn(Arc::clone(&ui), move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    // --- the core dies ----------------------------------------------------
    daemon.kill_hard();
    assert!(!daemon.is_alive());

    // Queries fail loudly instead of inventing state.
    let error = ui.get_edge_state().unwrap_err();
    assert!(error.is_core_unavailable(), "{error:?}");
    assert_eq!(error.code(), edge_protocol::error_codes::CORE_UNAVAILABLE);
    assert!(!ui.connection().connected);

    // Mutating commands are refused, and are never retried behind our back.
    let error = ui.totalize(None).unwrap_err();
    assert!(error.is_core_unavailable(), "{error:?}");

    // --- the core comes back ----------------------------------------------
    let mut daemon = DaemonProcess::start(&database);
    let state = wait_until_connected(|| ui.get_edge_state(), WAIT);
    let active = state
        .active_transaction
        .as_ref()
        .expect("the sale must still be there");
    assert_eq!(active.transaction_id, sale.transaction_id);
    assert_eq!(active.lines.len(), 1);
    assert_eq!(active.status, "OPEN");

    // Commands work again.
    let before = seen.load(Ordering::SeqCst);
    wait_until_connected(|| ui.totalize(None), WAIT);

    // The listener re-subscribes on its own schedule, so it may have missed the
    // change above. Keep making changes until a pushed event arrives, which is
    // what proves the stream came back rather than that we got lucky.
    // TOTALIZE is safe to repeat: it recomputes the same official totals.
    let deadline = std::time::Instant::now() + WAIT;
    while seen.load(Ordering::SeqCst) == before && std::time::Instant::now() < deadline {
        let _ = ui.totalize(None);
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        seen.load(Ordering::SeqCst) > before,
        "the event listener must reconnect and keep delivering events"
    );

    listener.stop();
    assert!(daemon.is_alive());
}

#[test]
fn a_second_runtime_refuses_to_start_on_the_same_installation() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let mut daemon = DaemonProcess::start(&database);
    let cli = client(&database, "edge-cli");
    open_register(&cli);

    let second = std::process::Command::new(env!("CARGO_BIN_EXE_edge-daemon"))
        .arg("--db")
        .arg(&database)
        .env("PI_EDGE_LOG", "error")
        .output()
        .unwrap();

    assert_eq!(
        second.status.code(),
        Some(edge_daemon::EXIT_SINGLE_WRITER),
        "a second runtime must exit cleanly with its own code"
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("already owns this installation"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains(&daemon.pid.to_string()),
        "the diagnostics should name the owning pid: {stderr}"
    );

    // The incumbent is untouched.
    assert!(daemon.is_alive());
    assert_eq!(cli.core_info().unwrap().pid, daemon.pid);
    assert!(cli.get_edge_state().unwrap().business_day.is_some());
}
