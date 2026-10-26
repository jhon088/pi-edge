//! Tauri bridge for the PI.Edge diagnostic screen.
//!
//! This crate is **an IPC client**, not the owner of the core. It holds an
//! `EdgeClient` and forwards calls to `edge-daemon`; it has no dependency on
//! `edge-application` or `edge-storage`, so it cannot open the database or
//! execute a use case even by mistake.
//!
//! ```text
//! React  ->  invoke()  ->  this bridge  ->  EdgeClient  ->  IPC  ->  edge-daemon
//! ```
//!
//! Closing the window stops this process and nothing else: the business day,
//! the open sale, the journal and the outbox stay with the daemon.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use edge_client::{ClientError, EdgeClient, EventListener, IpcEdgeClient};
use edge_domain::prelude::Timestamp;
use edge_protocol::{
    CashPositionV1, CatalogStatusV1, CoreInfoV1, EdgeStateV1, EventV1, HardwareStatusV1,
    InventoryPositionV1, InventoryQueryV1, ProductLookupV1, ProductSelectorV1, RecentSalesV1,
    RefundEligibilityV1, RefundV1, RetailTransactionV1, SubmitScanRequestV1, SyncStatusV1,
    TransactionPaymentsV1, TransactionRequestV1,
};
use serde::Serialize;
use tauri::{Emitter, Manager};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

mod bootstrap;
mod kiosk;
mod scanner;

/// Event name the webview listens on.
const STATE_CHANGED_EVENT: &str = "edge://state-changed";

/// Where the installation is when `PI_EDGE_DB` says nothing.
///
/// Deliberately **not** `"./data/pi-edge.db"`. A relative default is resolved
/// against the process working directory, and `pnpm tauri dev` runs this
/// binary from `apps/tauri-pos/src-tauri` — so the window came up pointed at
/// `apps/tauri-pos/src-tauri/data/.pi-edge/d.sock` while the daemon listened
/// on `<repo>/data/.pi-edge/d.sock`. Every command then failed against a
/// socket nothing had ever created, and the fix people reached for was a
/// symlink. The working directory is not an input here; anchor the path.
fn default_database() -> PathBuf {
    if cfg!(debug_assertions) {
        // Development: the checkout this binary was compiled from. That is
        // where `cargo run -p edge-daemon -- --db ./data/pi-edge.db` puts the
        // database, and it is known at compile time, so no working directory
        // and no symlink takes part in the answer.
        return PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("data")
            .join("pi-edge.db");
    }
    // Installed: beside the executable. One layout, no second convention.
    match std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("data").join("pi-edge.db")))
    {
        Some(path) => path,
        None => PathBuf::from("data").join("pi-edge.db"),
    }
}

/// Failure shape handed to the webview. `code` is a stable contract; `message`
/// is for humans and must not be parsed.
#[derive(Debug, Clone, Serialize)]
pub struct BridgeError {
    pub code: String,
    pub message: String,
}

impl From<ClientError> for BridgeError {
    fn from(error: ClientError) -> Self {
        Self {
            code: error.code().to_owned(),
            message: error.to_string(),
        }
    }
}

/// Connection view for the diagnostic header.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionStatusDto {
    pub connected: bool,
    pub endpoint: String,
    pub mode: String,
    pub reconnect_in_ms: Option<u64>,
    /// Whether a mutating command may be offered to the operator at all.
    ///
    /// This screen exposes none yet; when it does, every such control must be
    /// gated on this flag so a disconnected UI can never pretend to sell.
    pub can_issue_commands: bool,
}

/// Managed state: one client, the background event listener, and the
/// keyboard-wedge scan buffer.
#[derive(Debug)]
pub struct EdgeBridge {
    client: Arc<IpcEdgeClient>,
    listener: Mutex<Option<EventListener>>,
    database: PathBuf,
    /// Per-window scan state. The *rule* for what a scan is lives in
    /// `edge-hardware`; this only holds the buffer (ADR 036).
    scanner: scanner::ScannerCapture,
}

impl EdgeBridge {
    fn connect(database: PathBuf) -> Result<Self, ClientError> {
        // Lazy: the client resolves the endpoint and credential now but does
        // not dial, so the window opens even with the core down.
        let client = IpcEdgeClient::for_database(&database, "tauri-pos")?;
        Ok(Self {
            client: Arc::new(client),
            listener: Mutex::new(None),
            database,
            scanner: scanner::ScannerCapture::default(),
        })
    }

    pub fn database(&self) -> &PathBuf {
        &self.database
    }
}

#[tauri::command]
fn core_info(bridge: tauri::State<'_, EdgeBridge>) -> Result<CoreInfoV1, BridgeError> {
    bridge.client.core_info().map_err(BridgeError::from)
}

#[tauri::command]
fn edge_state(bridge: tauri::State<'_, EdgeBridge>) -> Result<EdgeStateV1, BridgeError> {
    bridge.client.get_edge_state().map_err(BridgeError::from)
}

#[tauri::command]
fn catalog_status(bridge: tauri::State<'_, EdgeBridge>) -> Result<CatalogStatusV1, BridgeError> {
    bridge.client.catalog_status().map_err(BridgeError::from)
}

/// Diagnostic lookup. Resolution, pricing and tax are the core's work; this
/// only forwards the answer.
#[tauri::command]
fn lookup_barcode(
    bridge: tauri::State<'_, EdgeBridge>,
    barcode: String,
) -> Result<ProductLookupV1, BridgeError> {
    bridge
        .client
        .lookup_product(ProductSelectorV1::Barcode(barcode))
        .map_err(BridgeError::from)
}

/// Every payment of one sale, with its attempt history.
///
/// A diagnostic read, nothing more: the bridge forwards the core's own
/// projection and adds no financial reasoning of its own. There is
/// deliberately no `start_payment`, `reconcile` or `reverse` command here —
/// this window is read-only in this milestone, and those are exercised through
/// the CLI.
#[tauri::command]
fn transaction_payments(
    bridge: tauri::State<'_, EdgeBridge>,
    transaction_id: String,
) -> Result<TransactionPaymentsV1, BridgeError> {
    bridge
        .client
        .get_transaction_payments(TransactionRequestV1 { transaction_id })
        .map_err(BridgeError::from)
}

/// What the drawer should contain, and the movements that explain it.
///
/// A diagnostic read. There is deliberately no command here for cash in, cash
/// out, a safe drop, a close or a reopen: this window is read-only in this
/// milestone and those are exercised through the CLI.
///
/// Note what this is *not*: it is not the payload a cashier's close path uses.
/// That one is `SessionCloseRequirementsV1`, which cannot carry an amount at
/// all (ADR 022). This window shows the expected cash because it is an
/// operational view, not a count sheet.
#[tauri::command]
fn cash_position(bridge: tauri::State<'_, EdgeBridge>) -> Result<CashPositionV1, BridgeError> {
    bridge.client.cash_position().map_err(BridgeError::from)
}

/// What the ledger says is here, and the movements that explain it.
///
/// A diagnostic read. There is deliberately no command here for a manual
/// movement or a bootstrap import: this window is read-only in this milestone
/// and those are exercised through the CLI.
///
/// `on_hand` is what *this* installation's ledger projects, not a store-wide
/// count (ADR 026). It can be negative, and the screen shows it as such: a
/// till that sold what it did not have on record is a reportable condition,
/// and hiding it behind a zero would be the one thing a diagnostic window must
/// not do (ADR 029).
#[tauri::command]
fn inventory_position(
    bridge: tauri::State<'_, EdgeBridge>,
    product_ref: String,
    movement_limit: i64,
) -> Result<InventoryPositionV1, BridgeError> {
    bridge
        .client
        .inventory_position(InventoryQueryV1 {
            product_ref,
            movement_limit,
            offset: 0,
        })
        .map_err(BridgeError::from)
}

/// What a closed sale can still give back, and the refunds it already has.
///
/// A diagnostic read. There is deliberately no command here for creating,
/// accepting, paying or reconciling a refund: this window is read-only in this
/// milestone and those are exercised through the CLI. Every refundable figure
/// is the core's — the UI computes nothing (ADR 031).
#[tauri::command]
fn refund_eligibility(
    bridge: tauri::State<'_, EdgeBridge>,
    transaction_id: String,
) -> Result<RefundEligibilityV1, BridgeError> {
    bridge
        .client
        .refund_eligibility(TransactionRequestV1 { transaction_id })
        .map_err(BridgeError::from)
}

/// Every refund of one sale, with its financial, inventory and fiscal state.
#[tauri::command]
fn transaction_refunds(
    bridge: tauri::State<'_, EdgeBridge>,
    transaction_id: String,
) -> Result<Vec<RefundV1>, BridgeError> {
    bridge
        .client
        .transaction_refunds(TransactionRequestV1 { transaction_id })
        .map_err(BridgeError::from)
}

/// What each device is and what the runtime knows about it.
///
/// A diagnostic read. The window shows a state, a last success and a last
/// error; it never learns a host, a port or a driver's internals (ADR 035).
#[tauri::command]
fn hardware_status(bridge: tauri::State<'_, EdgeBridge>) -> Result<HardwareStatusV1, BridgeError> {
    bridge.client.hardware_status().map_err(BridgeError::from)
}

/// Recent sales with their receipt state.
///
/// A diagnostic read, and read-only: the window observes what a sale owes in
/// paper and never asks for a print. Driving a receipt is the runtime's job
/// (ADR 043).
#[tauri::command]
fn recent_sales(
    bridge: tauri::State<'_, EdgeBridge>,
    limit: i64,
) -> Result<RecentSalesV1, BridgeError> {
    bridge
        .client
        .list_recent_sales(limit, 0)
        .map_err(BridgeError::from)
}

/// What sync knows: how much is waiting, how much is stuck, how far behind
/// this till is.
///
/// A diagnostic read, and read-only: the window never triggers a dispatch
/// cycle and never learns a host, a URL or a credential — only an adapter
/// name (ADR 040).
#[tauri::command]
fn sync_status(bridge: tauri::State<'_, EdgeBridge>) -> Result<SyncStatusV1, BridgeError> {
    bridge.client.get_sync_status().map_err(BridgeError::from)
}

/// Print a diagnostic page. Safe by construction: it touches no commercial
/// data and the payload carries no bytes and no address (ADR 039).
#[tauri::command]
fn print_test_page(bridge: tauri::State<'_, EdgeBridge>) -> Result<HardwareStatusV1, BridgeError> {
    bridge
        .client
        .print_test_page(Some(Uuid::new_v4()))
        .map_err(BridgeError::from)
}

/// Open the drawer because the operator asked, from the diagnostic surface.
#[tauri::command]
fn open_drawer(bridge: tauri::State<'_, EdgeBridge>) -> Result<HardwareStatusV1, BridgeError> {
    bridge
        .client
        .open_drawer(Some(Uuid::new_v4()))
        .map_err(BridgeError::from)
}

/// One keystroke from a keyboard-wedge scanner.
///
/// The webview forwards every key it sees; the parser in `edge-hardware`
/// decides when a scan completed, and only then does anything reach the core.
/// Returns the sale when an item was added, and `None` while a barcode is
/// still being read — so React learns nothing it could act on until the core
/// has already acted (ADR 036).
#[tauri::command]
fn scanner_key(
    bridge: tauri::State<'_, EdgeBridge>,
    key: String,
) -> Result<Option<RetailTransactionV1>, BridgeError> {
    // The host stamps the keystroke; the parser compares gaps, never a wall
    // clock of its own.
    let now = Timestamp::from_datetime(chrono::Utc::now());
    let Some(barcode) = bridge.scanner.push(&key, now) else {
        return Ok(None);
    };
    bridge
        .client
        .submit_scan(SubmitScanRequestV1 {
            // A fresh id per physical scan: two scans of one product are two
            // units, and a resend of *this* scan is one.
            event_id: Uuid::new_v4().to_string(),
            barcode,
            device_id: None,
        })
        .map(Some)
        .map_err(BridgeError::from)
}

#[tauri::command]
fn connection_status(bridge: tauri::State<'_, EdgeBridge>) -> ConnectionStatusDto {
    let status = bridge.client.connection();
    ConnectionStatusDto {
        connected: status.connected,
        endpoint: status.endpoint,
        mode: status.mode.to_owned(),
        reconnect_in_ms: status.reconnect_in_ms,
        can_issue_commands: status.connected,
    }
}

/// Start the diagnostic window.
pub fn run() {
    init_tracing();

    // Two ways in. A development run points at a database somebody already
    // started a daemon on. An installed kiosk has no terminal and nobody to
    // use one, so the window brings the runtime up itself.
    let configured = std::env::var_os("PI_EDGE_DB").map(PathBuf::from);
    let installed = configured.is_none() && !cfg!(debug_assertions);

    let result = tauri::Builder::default()
        .manage(kiosk::AssetCache::default())
        .manage(bootstrap::CoreProcess::default())
        .setup(move |app| {
            // The database is only known here, because on an installed kiosk
            // it lives in a directory Tauri resolves for the platform. The
            // bridge is therefore built here too, once, against the path
            // actually in use — never rebuilt, because `manage` keeps the
            // first value of a type and silently drops a second.
            let database = match (&configured, installed) {
                (Some(path), _) => path.clone(),
                (None, true) => {
                    let data = bootstrap::data_dir(app.handle())?;
                    log_to_data_dir(&data);
                    data.join("pi-edge.db")
                }
                (None, false) => default_database(),
            };

            if installed {
                // Everything a person would otherwise do at a prompt: start
                // PI.Edge, and install the demo the first time only.
                let owned = app.state::<bootstrap::CoreProcess>();
                bootstrap::ensure_core(&database, &owned)?;
                bootstrap::seed_once(&database, &bootstrap::bundled_demo(app.handle()))?;
            }

            let bridge = EdgeBridge::connect(database.clone()).map_err(|error| {
                // A kiosk that opens onto nothing is worse than one that says
                // why, so this is fatal and the reason is logged.
                tracing::error!(
                    database = %database.display(),
                    %error,
                    "PX Kiosk could not reach PI.Edge"
                );
                error
            })?;
            // The resolved endpoint, not the path as written. If this line and
            // the daemon's "edge daemon ready" line disagree, they are talking
            // past each other and nothing else in the window will work.
            tracing::info!(
                database = %bridge.database().display(),
                endpoint = %bridge.client.connection().endpoint,
                "PX Kiosk starting"
            );
            app.manage(bridge);
            forward_core_events(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                // Never leave the core running without a window: the daemon
                // this process started goes with it. One it adopted does not.
                use tauri::Manager;
                window
                    .app_handle()
                    .state::<bootstrap::CoreProcess>()
                    .shutdown();
            }
        })
        .invoke_handler(tauri::generate_handler![
            core_info,
            edge_state,
            catalog_status,
            lookup_barcode,
            transaction_payments,
            cash_position,
            inventory_position,
            refund_eligibility,
            transaction_refunds,
            hardware_status,
            sync_status,
            recent_sales,
            print_test_page,
            open_drawer,
            scanner_key,
            connection_status,
            kiosk::kiosk_menu,
            kiosk::kiosk_product_configuration,
            kiosk::kiosk_active_transaction,
            kiosk::kiosk_start_transaction,
            kiosk::kiosk_add_item,
            kiosk::kiosk_remove_item,
            kiosk::kiosk_change_quantity,
            kiosk::kiosk_cancel_transaction,
            kiosk::kiosk_totalize,
            kiosk::kiosk_pay,
            kiosk::kiosk_receipt,
            kiosk::kiosk_asset,
            kiosk::admin_printer_settings,
            kiosk::admin_save_printer_settings,
            kiosk::admin_test_print,
            kiosk::admin_print_receipt,
            kiosk::admin_reset_demo
        ])
        .run(tauri::generate_context!());

    if let Err(error) = result {
        tracing::error!(%error, "the diagnostic window exited with an error");
    }
}

/// Relay core notifications into the webview.
///
/// The event carries no business data on purpose: the screen answers it by
/// re-querying the core, so it can never drift from the runtime.
fn forward_core_events(app: &tauri::AppHandle) {
    let bridge = app.state::<EdgeBridge>();
    let handle = app.clone();
    let listener = EventListener::spawn(Arc::clone(&bridge.client), move |envelope| match envelope
        .event
    {
        EventV1::EdgeStateChanged(payload) => {
            if let Err(error) = handle.emit(STATE_CHANGED_EVENT, payload) {
                tracing::warn!(%error, "could not emit a core event to the webview");
            }
        }
    });
    if let Ok(mut slot) = bridge.listener.lock() {
        *slot = Some(listener);
    };
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("PI_EDGE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

/// Also write the log to app-data once the data directory is known.
///
/// An installed kiosk has no terminal, so a startup that fails has nowhere to
/// have said so. Appends; never rotates by itself, because a kiosk that
/// cannot start is the only thing that writes much here.
fn log_to_data_dir(data: &std::path::Path) {
    let dir = data.join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("px-kiosk.log");
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_env("PI_EDGE_LOG")
                        .unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .with_writer(std::sync::Mutex::new(file))
                .with_ansi(false)
                .with_target(false)
                .try_init();
            tracing::info!(log = %path.display(), "writing a startup log");
        }
        Err(error) => tracing::warn!(%error, path = %path.display(), "no startup log"),
    }
}
