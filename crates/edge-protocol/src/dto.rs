//! Version 1 data transfer objects.
//!
//! Pure `serde` types: no domain, no application, no transport. Conversions to
//! and from the core live in [`crate::mapping`], behind the `core-mapping`
//! feature.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A monetary amount as seen by clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoneyV1 {
    pub minor: i64,
    pub currency: String,
    /// Core-rendered representation. Clients display this verbatim.
    pub formatted: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceV1 {
    pub tenant_id: String,
    pub store_id: String,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessDayV1 {
    pub business_day_id: String,
    pub tenant_id: String,
    pub store_id: String,
    pub status: String,
    pub opened_at: String,
    pub closed_at: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterSessionV1 {
    pub session_id: String,
    pub business_day_id: String,
    pub register_id: String,
    pub operator_id: String,
    pub status: String,
    pub opening_cash: MoneyV1,
    pub opened_at: String,
    pub closed_at: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionLineV1 {
    pub line_id: String,
    pub line_number: i64,
    pub product_ref: String,
    pub sku: Option<String>,
    pub barcode: Option<String>,
    pub description: String,
    pub quantity: String,
    pub quantity_thousandths: i64,
    pub unit_price: MoneyV1,
    pub tax_rate_bps: u32,
    pub gross_amount: MoneyV1,
    pub discount_amount: MoneyV1,
    pub taxable_amount: MoneyV1,
    pub tax_amount: MoneyV1,
    pub line_total: MoneyV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TotalsV1 {
    pub subtotal: MoneyV1,
    pub discount_total: MoneyV1,
    pub tax_total: MoneyV1,
    pub grand_total: MoneyV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentV1 {
    pub payment_id: String,
    pub tender_type: String,
    pub amount: MoneyV1,
    pub status: String,
    pub created_at: String,
    pub committed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetailTransactionV1 {
    pub transaction_id: String,
    pub business_day_id: String,
    pub session_id: String,
    pub store_id: String,
    pub register_id: String,
    pub operator_id: String,
    pub register_sequence: i64,
    pub status: String,
    pub currency: String,
    pub lines: Vec<TransactionLineV1>,
    pub totals: TotalsV1,
    pub payments: Vec<PaymentV1>,
    /// Derived by the core: committed payments.
    pub paid_total: MoneyV1,
    /// Derived by the core: what is still owed.
    pub balance_due: MoneyV1,
    /// Derived by the core: cash owed back to the customer.
    pub change_due: MoneyV1,
    pub created_at: String,
    pub updated_at: String,
    pub finalized_at: Option<String>,
    pub version: i64,
}

/// Diagnostic snapshot of the core: what `get-state` prints and what the Tauri
/// diagnostic screen renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeStateV1 {
    pub protocol_version: u32,
    pub core_version: String,
    pub core_status: String,
    pub device: Option<DeviceV1>,
    pub business_day: Option<BusinessDayV1>,
    pub session: Option<RegisterSessionV1>,
    pub active_transaction: Option<RetailTransactionV1>,
    pub pending_outbox: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEventV1 {
    pub event_id: String,
    pub event_type: String,
    pub aggregate_id: String,
    pub aggregate_version: i64,
    pub status: String,
    pub attempt_count: i64,
    pub occurred_at: String,
    pub next_attempt_at: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOutboxV1 {
    pub protocol_version: u32,
    pub pending_count: i64,
    pub events: Vec<OutboxEventV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEventV1 {
    pub event_id: String,
    pub event_type: String,
    pub aggregate_id: String,
    pub business_day_id: Option<String>,
    pub session_id: Option<String>,
    pub transaction_id: Option<String>,
    pub operator_id: Option<String>,
    pub occurred_at: String,
    pub payload: Value,
}

/// Lifecycle state of the daemon runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CoreStatusV1 {
    /// Acquiring the single-writer lock, opening and migrating the database.
    Starting,
    /// Serving requests.
    Ready,
    /// Serving, but the last database operation failed.
    Degraded,
    /// Draining in-flight work; no new connections are accepted.
    Stopping,
}

impl CoreStatusV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            CoreStatusV1::Starting => "STARTING",
            CoreStatusV1::Ready => "READY",
            CoreStatusV1::Degraded => "DEGRADED",
            CoreStatusV1::Stopping => "STOPPING",
        }
    }

    /// Whether mutating operations may be accepted in this state.
    pub const fn accepts_commands(self) -> bool {
        matches!(self, CoreStatusV1::Ready | CoreStatusV1::Degraded)
    }
}

/// Answer to `GET_CORE_INFO`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreInfoV1 {
    pub core_version: String,
    pub protocol_version: u32,
    pub status: CoreStatusV1,
    pub database_schema_version: i64,
    pub uptime_seconds: u64,
    /// Monotonic within one daemon lifetime; bumped on every applied command.
    /// Diagnostic staleness signal only — never an aggregate version and never
    /// an input to business logic.
    pub revision: u64,
    pub pid: u32,
}

// ------------------------------------------------------------ request payloads

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitDeviceRequestV1 {
    pub tenant_id: String,
    pub store_id: String,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenRegisterSessionRequestV1 {
    pub register_id: String,
    pub operator_id: String,
    pub opening_cash_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddItemRequestV1 {
    /// Canonical catalog reference. PI.Edge never creates products.
    pub product_ref: String,
    pub sku: Option<String>,
    pub barcode: Option<String>,
    pub description: String,
    pub quantity_thousandths: i64,
    pub unit_price_minor: i64,
    pub tax_rate_bps: u32,
    #[serde(default)]
    pub discount_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitCashPaymentRequestV1 {
    pub amount_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitRequestV1 {
    pub limit: i64,
}
