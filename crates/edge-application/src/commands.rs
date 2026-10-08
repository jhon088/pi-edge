//! Command inputs for the mutating use cases.
//!
//! Every command carries an optional `request_id`: when present, the core
//! records the command result and a replay returns that result instead of
//! applying the command twice (explicit idempotency).

use edge_catalog::{
    CatalogBarcode, CatalogProduct, PriceBook, PriceEntry, ProductSelector, SnapshotCounts,
    TaxProfile,
};
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

/// Identity and quantity only.
///
/// Description, price, tax rate and price mode are resolved by the core from
/// the snapshot the sale captured. A client cannot state a price, so it cannot
/// be wrong about one.
#[derive(Debug, Clone)]
pub struct AddItemCommand {
    pub request_id: Option<Uuid>,
    pub selector: ProductSelector,
    pub quantity: Quantity,
}

/// Open an import and declare what is coming.
///
/// Idempotent on `(store, version, declared_content_hash)`: calling it again
/// returns the open import and its progress, which is what makes a resume
/// after a crash or a disconnect a normal call rather than a special case.
#[derive(Debug, Clone)]
pub struct BeginCatalogImportCommand {
    pub request_id: Option<Uuid>,
    /// Monotonic version assigned upstream.
    pub version: i64,
    /// Currency the snapshot quotes prices in.
    pub currency: Currency,
    pub declared_content_hash: String,
    pub generated_at: Timestamp,
    pub effective_at: Option<Timestamp>,
    pub expected: SnapshotCounts,
    /// Few in number, so they travel with the header and let every chunk be
    /// validated against them immediately.
    pub tax_profiles: Vec<TaxProfile>,
    /// Likewise: a handful of books, and every price entry in every chunk has
    /// to name one of them.
    pub price_books: Vec<PriceBook>,
}

/// One durable, idempotent slice of catalog content.
///
/// No `request_id`: `(snapshot_id, chunk_index, chunk_hash)` is a stronger
/// idempotency key, and it avoids writing a command record per chunk.
#[derive(Debug, Clone)]
pub struct PutCatalogChunkCommand {
    pub snapshot_id: CatalogSnapshotId,
    pub chunk_index: i64,
    /// Hash of this chunk's canonical content, computed by the client.
    pub chunk_hash: String,
    pub products: Vec<CatalogProduct>,
    /// Prices for the products in this chunk, so a product and its prices
    /// always land in the same transaction.
    pub price_entries: Vec<PriceEntry>,
    pub barcodes: Vec<CatalogBarcode>,
}

/// Close the import. Everything is verified before the snapshot becomes
/// usable, and nothing becomes partially usable.
#[derive(Debug, Clone)]
pub struct CommitCatalogImportCommand {
    pub request_id: Option<Uuid>,
    pub snapshot_id: CatalogSnapshotId,
    /// How many chunks the client sent, so a lost tail is detected.
    pub chunk_count: i64,
}

#[derive(Debug, Clone)]
pub struct AbortCatalogImportCommand {
    pub request_id: Option<Uuid>,
    pub snapshot_id: CatalogSnapshotId,
}

/// Put a committed snapshot in force. Atomic by construction.
#[derive(Debug, Clone)]
pub struct ActivateCatalogSnapshotCommand {
    pub request_id: Option<Uuid>,
    pub snapshot_id: CatalogSnapshotId,
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
