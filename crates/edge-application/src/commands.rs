//! Command inputs for the mutating use cases.
//!
//! Every command carries an optional `request_id`: when present, the core
//! records the command result and a replay returns that result instead of
//! applying the command twice (explicit idempotency).

use edge_domain::prelude::*;
use uuid::Uuid;

/// Provision the local device identity (normally pushed by the server).
#[derive(Debug, Clone)]
pub struct InitDeviceCommand {
    pub tenant_id: TenantId,
    pub store_id: StoreId,
    pub currency: Currency,
}

#[derive(Debug, Clone, Default)]
pub struct OpenBusinessDayCommand {
    pub request_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct OpenRegisterSessionCommand {
    pub request_id: Option<Uuid>,
    pub register_id: RegisterId,
    pub operator_id: OperatorId,
    pub opening_cash_minor: i64,
}

#[derive(Debug, Clone, Default)]
pub struct StartTransactionCommand {
    pub request_id: Option<Uuid>,
}

/// Pricing inputs are supplied by the caller in this milestone. They will be
/// replaced by the Catalog Snapshot + Pricing/Tax/Promotions engines without
/// changing the transaction aggregate.
#[derive(Debug, Clone)]
pub struct AddItemCommand {
    pub request_id: Option<Uuid>,
    pub product_ref: ProductRef,
    pub sku: Option<String>,
    pub barcode: Option<String>,
    pub description: String,
    pub quantity: Quantity,
    pub unit_price_minor: i64,
    pub tax_rate: TaxRate,
    pub discount_minor: i64,
}

#[derive(Debug, Clone, Default)]
pub struct TotalizeTransactionCommand {
    pub request_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct CommitCashPaymentCommand {
    pub request_id: Option<Uuid>,
    pub amount_minor: i64,
}

#[derive(Debug, Clone, Default)]
pub struct FinalizeTransactionCommand {
    pub request_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct CloseRegisterSessionCommand {
    pub request_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct CloseBusinessDayCommand {
    pub request_id: Option<Uuid>,
}
