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

mod error;
mod render;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use edge_client::{EdgeClient, IpcEdgeClient};
use edge_protocol::{
    AddItemRequestV1, CommitCashPaymentRequestV1, InitDeviceRequestV1,
    OpenRegisterSessionRequestV1, PROTOCOL_VERSION,
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
    /// Add a priced item to the active transaction.
    AddItem {
        /// Canonical catalog reference.
        #[arg(long)]
        product: String,
        #[arg(long)]
        description: String,
        /// Unit price in minor units.
        #[arg(long)]
        price: i64,
        /// Tax rate in basis points (1600 = 16.00%).
        #[arg(long = "tax-rate", default_value_t = 0)]
        tax_rate: u32,
        /// Quantity in whole units.
        #[arg(long, default_value_t = 1)]
        qty: i64,
        /// Quantity in thousandths, for fractional units. Overrides --qty.
        #[arg(long = "qty-thousandths")]
        qty_thousandths: Option<i64>,
        #[arg(long)]
        sku: Option<String>,
        #[arg(long)]
        barcode: Option<String>,
        /// Line discount in minor units.
        #[arg(long, default_value_t = 0)]
        discount: i64,
    },
    /// Compute the official totals.
    Total,
    /// Commit a cash tender in minor units.
    PayCash {
        #[arg(long)]
        amount: i64,
    },
    /// Finalize the sale (transaction + journal + outbox, atomically).
    FinalizeSale,
    /// Print the full edge state, rebuilt by the core from SQLite.
    GetState,
    /// Print the active transaction, if any.
    ActiveSale,
    /// Close the active register session.
    CloseSession,
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
        } => {
            let device = client.init_device(InitDeviceRequestV1 {
                tenant_id: tenant.clone(),
                store_id: store.clone(),
                currency: currency.clone(),
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
            description,
            price,
            tax_rate,
            qty,
            qty_thousandths,
            sku,
            barcode,
            discount,
        } => {
            let quantity_thousandths = qty_thousandths.unwrap_or(qty.saturating_mul(1_000));
            to_value(&client.add_item(
                AddItemRequestV1 {
                    product_ref: product.clone(),
                    sku: sku.clone(),
                    barcode: barcode.clone(),
                    description: description.clone(),
                    quantity_thousandths,
                    unit_price_minor: *price,
                    tax_rate_bps: *tax_rate,
                    discount_minor: *discount,
                },
                request_id,
            )?)
        }
        Command::Total => to_value(&client.totalize(request_id)?),
        Command::PayCash { amount } => to_value(&client.commit_cash_payment(
            CommitCashPaymentRequestV1 {
                amount_minor: *amount,
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
        Command::CloseSession => to_value(&client.close_session(request_id)?),
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
