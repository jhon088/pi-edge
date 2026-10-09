//! `edge-cli` — command line client of the PI.Edge core.
//!
//! It owns no business logic and no database handle. It parses arguments,
//! calls `EdgeClient`, and prints the protocol DTO that comes back:
//!
//! ```text
//! edge-cli -> EdgeClient -> IPC -> edge-daemon -> core -> SQLite
//! ```
//!
//! The Tauri UI uses the same `EdgeClient` against the same daemon, which is
//! how we know the contract is real and not CLI-shaped.

mod catalog;
mod error;
mod inventory;
mod refund;
mod render;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use edge_client::{EdgeClient, IpcEdgeClient};
use edge_protocol::{
    AddItemRequestV1, ApproveSafeDropRequestV1, CashMovementRequestV1, ChangeQuantityRequestV1,
    CreateRefundRequestV1, DeclareClosingCashRequestV1, ForceCloseBusinessDayRequestV1,
    InitDeviceRequestV1, InventoryMovementRequestV1, InventoryQueryV1,
    OpenRegisterSessionRequestV1, PaymentRequestV1, PrintTransactionRequestV1, ProductSelectorV1,
    RefundRequestV1, RemoveItemRequestV1, ReopenSessionRequestV1, StartPaymentRequestV1,
    SubmitScanRequestV1, TransactionRequestV1, PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use crate::error::CliError;
use crate::render::{render, OutputFormat};

#[derive(Debug, Parser)]
#[command(
    name = "edge-cli",
    version,
    about = "PI.Edge headless POS core — command line client (talks to edge-daemon)"
)]
struct Cli {
    /// Installation to talk to. Only used to locate the daemon's endpoint and
    /// local credential; the CLI never opens the database.
    #[arg(
        long,
        env = "PI_EDGE_DB",
        default_value = "./data/pi-edge.db",
        global = true
    )]
    db: PathBuf,

    /// Idempotency key. Replaying the same key returns the recorded result
    /// instead of applying the command twice.
    #[arg(long, global = true)]
    request_id: Option<Uuid>,

    #[arg(long, value_enum, default_value_t = OutputFormat::Text, global = true)]
    format: OutputFormat,

    #[command(subcommand)]
    command: Command,
}

/// How to name a product. Exactly one, required.
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct ProductArgs {
    /// Scanned barcode.
    #[arg(long)]
    barcode: Option<String>,
    /// Stock keeping unit.
    #[arg(long)]
    sku: Option<String>,
    /// Canonical catalog reference assigned upstream.
    #[arg(long)]
    product: Option<String>,
}

impl ProductArgs {
    fn to_selector(&self) -> Result<ProductSelectorV1, CliError> {
        match (&self.barcode, &self.sku, &self.product) {
            (Some(barcode), None, None) => Ok(ProductSelectorV1::Barcode(barcode.clone())),
            (None, Some(sku), None) => Ok(ProductSelectorV1::Sku(sku.clone())),
            (None, None, Some(product)) => Ok(ProductSelectorV1::ProductRef(product.clone())),
            _ => Err(CliError::Input(
                "name exactly one of --barcode, --sku or --product".to_owned(),
            )),
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Runtime identity, status, schema version and uptime.
    CoreInfo,
    /// Provision the local device identity (tenant/store/currency).
    InitDemo {
        #[arg(long, default_value = "TENANT-DEMO")]
        tenant: String,
        #[arg(long, default_value = "STORE-DEMO")]
        store: String,
        #[arg(long, default_value = "MXN")]
        currency: String,
        /// Refuse a sale the ledger cannot cover. Off by default: a stale
        /// position must not stop the counter (ADR 029).
        #[arg(long)]
        block_negative_inventory: bool,
    },
    /// Open the business day for this store.
    OpenBusinessDay,
    /// Open a register session.
    OpenSession {
        #[arg(long)]
        register: String,
        #[arg(long)]
        operator: String,
        /// Opening float in minor units (e.g. 100000 = 1000.00 MXN).
        #[arg(long = "opening-cash", default_value_t = 0)]
        opening_cash: i64,
    },
    /// Start a new retail transaction.
    StartSale,
    /// Add an item to the active transaction.
    ///
    /// The core resolves description, price, tax rate and price mode from the
    /// snapshot this sale captured. A client cannot state a price.
    AddItem {
        #[command(flatten)]
        product: ProductArgs,
        /// Quantity in whole units.
        #[arg(long, default_value_t = 1)]
        qty: i64,
        /// Quantity in thousandths, for fractional units. Overrides --qty.
        #[arg(long = "qty-thousandths")]
        qty_thousandths: Option<i64>,
    },
    /// Remove a line from the active transaction, by line number.
    RemoveItem {
        #[arg(long = "line")]
        line: i64,
    },
    /// Change a line's quantity. The line keeps its captured unit price.
    ChangeQty {
        #[arg(long = "line")]
        line: i64,
        /// New quantity in whole units.
        #[arg(long, default_value_t = 1)]
        qty: i64,
        /// New quantity in thousandths, for fractional units. Overrides --qty.
        #[arg(long = "qty-thousandths")]
        qty_thousandths: Option<i64>,
    },
    /// Import a catalog snapshot and stream it to the core in chunks.
    ImportCatalog {
        /// Snapshot file produced upstream (a fixture, for now).
        file: PathBuf,
        /// Activate the snapshot once it is committed.
        #[arg(long)]
        activate: bool,
        /// Per-chunk byte budget. A client policy, not a protocol rule.
        #[arg(long = "chunk-bytes")]
        chunk_bytes: Option<usize>,
    },
    /// Active, pending and in-flight catalog snapshots.
    CatalogStatus,
    /// Progress of an open import, for resuming it.
    ImportStatus {
        #[arg(long = "snapshot")]
        snapshot_id: String,
    },
    /// Activate a committed snapshot.
    ActivateCatalog {
        #[arg(long = "snapshot")]
        snapshot_id: String,
    },
    /// Discard an open import.
    AbortCatalog {
        #[arg(long = "snapshot")]
        snapshot_id: String,
    },
    /// Resolve a product the way a sale would, without touching a sale.
    Lookup {
        #[command(flatten)]
        product: ProductArgs,
    },
    /// Compute the official totals.
    Total,
    /// Start a payment for the active sale.
    ///
    /// Cash settles immediately and may overpay, producing change. A card
    /// goes through its provider and may come back unresolved, in which case
    /// the only way forward is `reconcile-payment`.
    StartPayment {
        /// `CASH` or `CARD`.
        #[arg(long, default_value = "CASH")]
        tender: String,
        /// What this tender covers, in minor units.
        #[arg(long)]
        amount: i64,
    },
    /// Every payment of the active sale, with its attempt history.
    Payments,
    /// Ask the provider what happened to an unresolved payment. Never charges.
    ReconcilePayment {
        #[arg(long = "payment")]
        payment_id: String,
    },
    /// Undo a payment that must not remain applied.
    ReversePayment {
        #[arg(long = "payment")]
        payment_id: String,
    },
    /// Finalize the sale (transaction + journal + outbox, atomically).
    FinalizeSale,
    /// Print the full edge state, rebuilt by the core from SQLite.
    GetState,
    /// Print the active transaction, if any.
    ActiveSale,
    /// What each device is and what the runtime knows about it.
    HardwareStatus,
    /// Print a diagnostic page. Touches no commercial data.
    PrinterTest,
    /// Open the drawer because you asked. Audited, and it opens every time.
    DrawerOpen,
    /// Print a finalized sale. `--reprint` marks it as a copy and still
    /// changes nothing: no cash, no stock, no drawer (ADR 037).
    PrintTransaction {
        #[arg(long = "transaction")]
        transaction_id: String,
        #[arg(long)]
        reprint: bool,
    },
    /// Feed a completed scan, as a host's parser would. Diagnostic: the real
    /// path is the scanner itself.
    Scan {
        #[arg(long)]
        barcode: String,
        /// The id of this physical scan. A fresh one each time, because two
        /// scans of the same product are two units (ADR 036).
        #[arg(long = "event")]
        event_id: Option<String>,
    },
    /// What a closed sale can still give back. The core computes it all.
    RefundEligibility {
        #[arg(long = "transaction")]
        transaction_id: String,
    },
    /// Open a refund of a closed sale.
    ///
    /// Authority over the original sale is resolved by the runtime, never
    /// asserted here (ADR 030).
    RefundCreate {
        #[arg(long = "transaction")]
        transaction_id: String,
        /// One per line, as `<original-line>:<thousandths>`, e.g. `1:2000`.
        #[arg(long = "line", required = true)]
        lines: Vec<String>,
        /// Why. A blank reason is refused.
        #[arg(long)]
        reason: String,
        /// `NONE`, `PENDING` or `REQUEST`. A sale that was never invoiced
        /// admits only `NONE` (ADR 032).
        #[arg(long = "fiscal-action", default_value = "NONE")]
        fiscal_action: String,
    },
    /// Record that the goods came back. Returns them to stock, once.
    RefundAccept {
        #[arg(long = "refund")]
        refund_id: String,
    },
    /// Give the money back, spread over the original tenders by the core.
    RefundPay {
        #[arg(long = "refund")]
        refund_id: String,
    },
    /// Ask the provider about an unresolved refund. Never refunds again.
    RefundReconcile {
        #[arg(long = "refund")]
        refund_id: String,
    },
    RefundShow {
        #[arg(long = "refund")]
        refund_id: String,
    },
    /// Every refund of one sale.
    Refunds {
        #[arg(long = "transaction")]
        transaction_id: String,
    },
    /// Import the opening stock baseline. One per installation, ever
    /// (ADR 028).
    InventoryImport {
        /// Opening-position file produced upstream (a fixture, for now).
        file: PathBuf,
    },
    /// What the ledger says is here, and the movements that explain it.
    InventoryPosition {
        #[arg(long = "product")]
        product_ref: String,
        /// How many movements to include. Zero for the position alone.
        #[arg(long, default_value_t = 10)]
        movements: i64,
    },
    /// One product's movements, oldest first.
    InventoryMovements {
        #[arg(long = "product")]
        product_ref: String,
        #[arg(long, default_value_t = 20)]
        limit: i64,
        #[arg(long, default_value_t = 0)]
        offset: i64,
    },
    /// Put stock in, with a stated reason.
    InventoryIn {
        #[arg(long = "product")]
        product_ref: String,
        /// Thousandths: `1500` is `1.500`.
        #[arg(long = "qty")]
        quantity_thousandths: i64,
        /// Why. A blank reason is refused.
        #[arg(long)]
        reason: String,
    },
    /// Take stock out, with a stated reason.
    InventoryOut {
        #[arg(long = "product")]
        product_ref: String,
        #[arg(long = "qty")]
        quantity_thousandths: i64,
        #[arg(long)]
        reason: String,
    },
    /// What the drawer should contain, and the movements that explain it.
    CashPosition,
    /// Put money into the drawer.
    CashIn {
        #[arg(long)]
        amount: i64,
        /// Why. A blank reason is refused.
        #[arg(long)]
        reason: String,
    },
    /// Take money out of the drawer.
    CashOut {
        #[arg(long)]
        amount: i64,
        #[arg(long)]
        reason: String,
    },
    /// Ask for money to be moved to the safe. Moves nothing on its own.
    SafeDrop {
        #[arg(long)]
        amount: i64,
        #[arg(long)]
        reason: String,
    },
    /// Authorise a pending safe drop. The approver must not be the requester.
    ApproveSafeDrop {
        #[arg(long = "id")]
        safe_drop_id: String,
        #[arg(long)]
        approver: String,
        /// Decline instead of approving.
        #[arg(long)]
        reject: bool,
    },
    /// Begin a session close. The answer never reveals the expected cash.
    RequestCloseSession,
    /// Declare the counted cash. Only now is the variance revealed.
    DeclareClose {
        #[arg(long)]
        amount: i64,
    },
    /// Continue a closed session under a new one.
    ReopenSession {
        #[arg(long = "session")]
        session_id: String,
        #[arg(long)]
        reason: String,
        /// Who takes over. Defaults to the demo cashier.
        #[arg(long, default_value = "CASHIER-001")]
        operator: String,
    },
    /// End the day over open sessions, under an explicit policy.
    ForceCloseBusinessDay {
        #[arg(long)]
        reason: String,
        /// Who or what ordered it.
        #[arg(long, default_value = "OPERATIONS")]
        actor: String,
    },
    /// Close the business day.
    CloseBusinessDay,
    /// Tail the local journal.
    Journal {
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// List undelivered outbox events.
    Outbox {
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// Print core events as they happen (same stream the UI consumes).
    Watch {
        /// Stop after this many events. 0 means run until interrupted.
        #[arg(long, default_value_t = 0)]
        count: usize,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing();
    match run(&cli) {
        Ok(value) => {
            print!("{}", render(&value, cli.format));
            ExitCode::SUCCESS
        }
        Err(error) => {
            let mut payload = json!({
                "error": {
                    "kind": error.kind(),
                    "message": error.to_string(),
                }
            });
            if let (Some(hint), Some(object)) = (error.hint(), payload["error"].as_object_mut()) {
                object.insert("hint".to_owned(), json!(hint));
            }
            eprint!("{}", render(&payload, cli.format));
            ExitCode::FAILURE
        }
    }
}

/// Logs go to stderr so stdout stays a clean protocol channel.
fn init_tracing() {
    let filter = EnvFilter::try_from_env("PI_EDGE_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}

fn run(cli: &Cli) -> Result<Value, CliError> {
    let client = IpcEdgeClient::for_database(&cli.db, "edge-cli")?;
    let request_id = cli.request_id;

    match &cli.command {
        Command::CoreInfo => to_value(&client.core_info()?),
        Command::InitDemo {
            tenant,
            store,
            currency,
            block_negative_inventory,
        } => {
            let device = client.init_device(InitDeviceRequestV1 {
                tenant_id: tenant.clone(),
                store_id: store.clone(),
                currency: currency.clone(),
                allow_negative_inventory: !*block_negative_inventory,
            })?;
            to_value(&json!({
                "protocol_version": PROTOCOL_VERSION,
                "device": device,
                "endpoint": client.connection().endpoint,
            }))
        }
        Command::OpenBusinessDay => to_value(&client.open_business_day(request_id)?),
        Command::OpenSession {
            register,
            operator,
            opening_cash,
        } => to_value(&client.open_session(
            OpenRegisterSessionRequestV1 {
                register_id: register.clone(),
                operator_id: operator.clone(),
                opening_cash_minor: *opening_cash,
            },
            request_id,
        )?),
        Command::StartSale => to_value(&client.start_transaction(request_id)?),
        Command::AddItem {
            product,
            qty,
            qty_thousandths,
        } => {
            let quantity_thousandths = qty_thousandths.unwrap_or(qty.saturating_mul(1_000));
            to_value(&client.add_item(
                AddItemRequestV1 {
                    product: product.to_selector()?,
                    quantity_thousandths,
                },
                request_id,
            )?)
        }
        Command::RemoveItem { line } => {
            to_value(&client.remove_item(RemoveItemRequestV1 { line_number: *line }, request_id)?)
        }
        Command::ChangeQty {
            line,
            qty,
            qty_thousandths,
        } => {
            let quantity_thousandths = qty_thousandths.unwrap_or(qty.saturating_mul(1_000));
            to_value(&client.change_quantity(
                ChangeQuantityRequestV1 {
                    line_number: *line,
                    quantity_thousandths,
                },
                request_id,
            )?)
        }
        Command::ImportCatalog {
            file,
            activate,
            chunk_bytes,
        } => {
            let outcome = catalog::import_catalog(
                &client,
                file,
                chunk_bytes.unwrap_or(catalog::DEFAULT_CHUNK_BUDGET_BYTES),
                *activate,
            )?;
            outcome.to_value()
        }
        Command::CatalogStatus => to_value(&client.catalog_status()?),
        Command::ImportStatus { snapshot_id } => {
            to_value(&client.catalog_import_status(snapshot_id)?)
        }
        Command::ActivateCatalog { snapshot_id } => {
            to_value(&client.activate_catalog_snapshot(snapshot_id, request_id)?)
        }
        Command::AbortCatalog { snapshot_id } => {
            client.abort_catalog_import(snapshot_id, request_id)?;
            to_value(&json!({ "aborted": snapshot_id }))
        }
        Command::Lookup { product } => to_value(&client.lookup_product(product.to_selector()?)?),
        Command::Total => to_value(&client.totalize(request_id)?),
        Command::StartPayment { tender, amount } => to_value(&client.start_payment(
            StartPaymentRequestV1 {
                tender_type: tender.to_uppercase(),
                amount_minor: *amount,
            },
            request_id,
        )?),
        Command::Payments => {
            // The active sale is the one a cashier is looking at; a payment
            // listing for any other sale is an audit query, not a till action.
            let state = client.get_edge_state()?;
            let Some(transaction) = state.active_transaction else {
                return Err(CliError::Input("no active sale".to_owned()));
            };
            to_value(&client.get_transaction_payments(TransactionRequestV1 {
                transaction_id: transaction.transaction_id,
            })?)
        }
        Command::ReconcilePayment { payment_id } => to_value(&client.reconcile_payment(
            PaymentRequestV1 {
                payment_id: payment_id.clone(),
            },
            request_id,
        )?),
        Command::ReversePayment { payment_id } => to_value(&client.reverse_payment(
            PaymentRequestV1 {
                payment_id: payment_id.clone(),
            },
            request_id,
        )?),
        Command::FinalizeSale => to_value(&client.finalize_transaction(request_id)?),
        Command::GetState => {
            let state = client.get_edge_state()?;
            let connection = client.connection();
            to_value(&json!({
                "ipc": {
                    "connected": connection.connected,
                    "endpoint": connection.endpoint,
                    "mode": connection.mode,
                    "reconnect_in_ms": connection.reconnect_in_ms,
                },
                "state": state,
            }))
        }
        Command::ActiveSale => to_value(&client.get_active_transaction()?),
        Command::HardwareStatus => to_value(&client.hardware_status()?),
        Command::PrinterTest => to_value(&client.print_test_page(request_id)?),
        Command::DrawerOpen => to_value(&client.open_drawer(request_id)?),
        Command::PrintTransaction {
            transaction_id,
            reprint,
        } => to_value(&client.print_transaction(
            PrintTransactionRequestV1 {
                transaction_id: transaction_id.clone(),
                reprint: *reprint,
            },
            request_id,
        )?),
        Command::Scan { barcode, event_id } => to_value(
            &client.submit_scan(SubmitScanRequestV1 {
                event_id: event_id
                    .clone()
                    .unwrap_or_else(|| Uuid::new_v4().to_string()),
                barcode: barcode.clone(),
                device_id: None,
            })?,
        ),
        Command::RefundEligibility { transaction_id } => {
            to_value(&client.refund_eligibility(TransactionRequestV1 {
                transaction_id: transaction_id.clone(),
            })?)
        }
        Command::RefundCreate {
            transaction_id,
            lines,
            reason,
            fiscal_action,
        } => to_value(&client.create_refund(
            CreateRefundRequestV1 {
                transaction_id: transaction_id.clone(),
                lines: refund::parse_lines(lines)?,
                reason_code: reason.clone(),
                fiscal_action: refund::parse_fiscal_action(fiscal_action)?,
            },
            request_id,
        )?),
        Command::RefundAccept { refund_id } => to_value(&client.accept_refund(
            RefundRequestV1 {
                refund_id: refund_id.clone(),
            },
            request_id,
        )?),
        Command::RefundPay { refund_id } => to_value(&client.start_refund_payment(
            RefundRequestV1 {
                refund_id: refund_id.clone(),
            },
            request_id,
        )?),
        Command::RefundReconcile { refund_id } => to_value(&client.reconcile_refund(
            RefundRequestV1 {
                refund_id: refund_id.clone(),
            },
            request_id,
        )?),
        Command::RefundShow { refund_id } => to_value(&client.get_refund(RefundRequestV1 {
            refund_id: refund_id.clone(),
        })?),
        Command::Refunds { transaction_id } => {
            to_value(&client.transaction_refunds(TransactionRequestV1 {
                transaction_id: transaction_id.clone(),
            })?)
        }
        Command::InventoryImport { file } => {
            to_value(&inventory::import_bootstrap(&client, file, request_id)?)
        }
        Command::InventoryPosition {
            product_ref,
            movements,
        } => to_value(&client.inventory_position(InventoryQueryV1 {
            product_ref: product_ref.clone(),
            movement_limit: *movements,
            offset: 0,
        })?),
        Command::InventoryMovements {
            product_ref,
            limit,
            offset,
        } => to_value(&client.inventory_movements(InventoryQueryV1 {
            product_ref: product_ref.clone(),
            movement_limit: *limit,
            offset: *offset,
        })?),
        Command::InventoryIn {
            product_ref,
            quantity_thousandths,
            reason,
        } => to_value(&client.inventory_in(
            InventoryMovementRequestV1 {
                product_ref: product_ref.clone(),
                quantity_thousandths: *quantity_thousandths,
                reason_code: reason.clone(),
            },
            request_id,
        )?),
        Command::InventoryOut {
            product_ref,
            quantity_thousandths,
            reason,
        } => to_value(&client.inventory_out(
            InventoryMovementRequestV1 {
                product_ref: product_ref.clone(),
                quantity_thousandths: *quantity_thousandths,
                reason_code: reason.clone(),
            },
            request_id,
        )?),
        Command::CashPosition => to_value(&client.cash_position()?),
        Command::CashIn { amount, reason } => to_value(&client.cash_in(
            CashMovementRequestV1 {
                amount_minor: *amount,
                reason_code: reason.clone(),
            },
            request_id,
        )?),
        Command::CashOut { amount, reason } => to_value(&client.cash_out(
            CashMovementRequestV1 {
                amount_minor: *amount,
                reason_code: reason.clone(),
            },
            request_id,
        )?),
        Command::SafeDrop { amount, reason } => to_value(&client.request_safe_drop(
            CashMovementRequestV1 {
                amount_minor: *amount,
                reason_code: reason.clone(),
            },
            request_id,
        )?),
        Command::ApproveSafeDrop {
            safe_drop_id,
            approver,
            reject,
        } => to_value(&client.approve_safe_drop(
            ApproveSafeDropRequestV1 {
                safe_drop_id: safe_drop_id.clone(),
                approver: approver.clone(),
                approve: !*reject,
            },
            request_id,
        )?),
        Command::RequestCloseSession => to_value(&client.request_session_close(request_id)?),
        Command::DeclareClose { amount } => to_value(&client.declare_closing_cash(
            DeclareClosingCashRequestV1 {
                counted_minor: *amount,
            },
            request_id,
        )?),
        Command::ReopenSession {
            session_id,
            reason,
            operator,
        } => to_value(&client.reopen_session(
            ReopenSessionRequestV1 {
                session_id: session_id.clone(),
                reason: reason.clone(),
                operator_id: operator.clone(),
            },
            request_id,
        )?),
        Command::ForceCloseBusinessDay { reason, actor } => {
            to_value(&client.force_close_business_day(
                ForceCloseBusinessDayRequestV1 {
                    reason: reason.clone(),
                    actor: actor.clone(),
                },
                request_id,
            )?)
        }
        Command::CloseBusinessDay => to_value(&client.close_business_day(request_id)?),
        Command::Journal { limit } => to_value(&json!({
            "protocol_version": PROTOCOL_VERSION,
            "events": client.get_journal(*limit)?,
        })),
        Command::Outbox { limit } => to_value(&client.get_pending_outbox(*limit)?),
        Command::Watch { count } => watch(&client, *count),
    }
}

/// Print events from the core's push stream. Demonstrates that the CLI and the
/// UI consume the same notifications.
fn watch(client: &IpcEdgeClient, count: usize) -> Result<Value, CliError> {
    let mut stream = client.subscribe()?;
    // The subscribe acknowledgement means the core has already registered this
    // stream, so nothing that happens after this line can be missed.
    println!(
        "{}",
        json!({ "watching": true, "endpoint": client.connection().endpoint })
    );
    let mut seen = Vec::new();
    loop {
        match stream.next_event() {
            Ok(Some(event)) => {
                let value = to_value(&event)?;
                println!("{value}");
                seen.push(value);
                if count > 0 && seen.len() >= count {
                    break;
                }
            }
            Ok(None) => break,
            Err(error) => return Err(CliError::Client(error.into())),
        }
    }
    to_value(&json!({ "events_received": seen.len() }))
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, CliError> {
    serde_json::to_value(value).map_err(|e| CliError::Serialization(e.to_string()))
}
