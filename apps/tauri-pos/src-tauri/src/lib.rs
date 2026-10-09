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
use edge_protocol::{
    CatalogStatusV1, CoreInfoV1, EdgeStateV1, EventV1, ProductLookupV1, ProductSelectorV1,
    TransactionPaymentsV1, TransactionRequestV1,
};
use serde::Serialize;
use tauri::{Emitter, Manager};
use tracing_subscriber::EnvFilter;

/// Event name the webview listens on.
const STATE_CHANGED_EVENT: &str = "edge://state-changed";

/// Default installation when `PI_EDGE_DB` is not set.
const DEFAULT_DATABASE: &str = "./data/pi-edge.db";

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

/// Managed state: one client and the background event listener.
#[derive(Debug)]
pub struct EdgeBridge {
    client: Arc<IpcEdgeClient>,
    listener: Mutex<Option<EventListener>>,
    database: PathBuf,
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

    let database = std::env::var_os("PI_EDGE_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DATABASE));

    let bridge = match EdgeBridge::connect(database) {
        Ok(bridge) => bridge,
        Err(error) => {
            tracing::error!(%error, "could not prepare the edge client");
            return;
        }
    };
    tracing::info!(
        database = %bridge.database().display(),
        endpoint = %bridge.client.connection().endpoint,
        "diagnostic client starting"
    );

    let result = tauri::Builder::default()
        .manage(bridge)
        .setup(|app| {
            forward_core_events(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            core_info,
            edge_state,
            catalog_status,
            lookup_barcode,
            transaction_payments,
            connection_status
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
