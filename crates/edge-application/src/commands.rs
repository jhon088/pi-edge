//! Command inputs for the mutating use cases.
//!
//! Every command carries an optional `request_id`: when present, the core
//! records the command result and a replay returns that result instead of
//! applying the command twice (explicit idempotency).

use edge_catalog::{
    CatalogBarcode, CatalogProduct, PriceBook, PriceEntry, ProductSelector, Promotion,
    SnapshotCounts, TaxProfile,
};
use edge_domain::prelude::*;
use edge_hardware::ScanEvent;
use edge_inventory::BootstrapItem;
use edge_payments::TenderType;
use edge_refunds::{RefundAuthorizationContext, RefundFiscalStatus, RefundLineRequest};
use uuid::Uuid;

/// Provision the local device identity (normally pushed by the server).
#[derive(Debug, Clone)]
pub struct InitDeviceCommand {
    pub tenant_id: TenantId,
    pub store_id: StoreId,
    pub currency: Currency,
    /// Whether this installation may sell what it does not have on record.
    /// Defaults to permissive: a stale position must not block the counter
    /// (ADR 029).
    pub allow_negative_inventory: bool,
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

#[derive(Debug, Clone)]
pub struct RemoveItemCommand {
    pub request_id: Option<Uuid>,
    /// The line to remove. Numbers are never reused, so this is unambiguous
    /// even after earlier removals.
    pub line_number: i64,
}

#[derive(Debug, Clone)]
pub struct ChangeQuantityCommand {
    pub request_id: Option<Uuid>,
    pub line_number: i64,
    pub quantity: Quantity,
}

/// Import the opening inventory baseline. One per installation, ever
/// (ADR 028).
#[derive(Debug, Clone)]
pub struct ImportInventoryBootstrapCommand {
    pub request_id: Option<Uuid>,
    /// The upstream snapshot this position came from. Also the idempotency
    /// key: the same id replayed is a no-op, a different one is refused.
    pub source_snapshot_id: String,
    pub generated_at: Timestamp,
    pub items: Vec<BootstrapItem>,
}

/// Put stock in, with a stated reason.
#[derive(Debug, Clone)]
pub struct InventoryInCommand {
    pub request_id: Option<Uuid>,
    pub product_ref: ProductRef,
    pub quantity: Quantity,
    pub reason_code: String,
}

/// Take stock out, with a stated reason.
#[derive(Debug, Clone)]
pub struct InventoryOutCommand {
    pub request_id: Option<Uuid>,
    pub product_ref: ProductRef,
    pub quantity: Quantity,
    pub reason_code: String,
}

// ---------------------------------------------------------------- refunds

/// Open a refund of a closed sale.
///
/// One command does the whole opening: it validates the authority, prices the
/// lines from the original sale's materialised economics and reserves the
/// quantities, all in one database transaction. Splitting validation from
/// reservation would be an invitation to race (ADR 030).
#[derive(Debug, Clone)]
pub struct CreateRefundCommand {
    pub request_id: Option<Uuid>,
    /// Evidence that the original sale may be refunded. The application layer
    /// resolves this; the domain never goes looking for it (ADR 030).
    pub authorization: RefundAuthorizationContext,
    pub lines: Vec<RefundLineRequest>,
    pub reason_code: String,
    /// What should happen fiscally. Constrained by the original sale: a sale
    /// that was never invoiced admits only `NONE` (ADR 032).
    pub fiscal_action: RefundFiscalStatus,
}

/// The goods came back. This is where the inventory return is written and
/// where the commercial refund becomes irrevocable (ADR 033).
#[derive(Debug, Clone)]
pub struct AcceptRefundCommand {
    pub request_id: Option<Uuid>,
    pub refund_id: RefundId,
}

/// Give the money back, spread over the original tenders.
///
/// No amount and no tender: the allocation is the core's single deterministic
/// policy over what each original tender actually took (ADR 031).
#[derive(Debug, Clone)]
pub struct StartRefundPaymentCommand {
    pub request_id: Option<Uuid>,
    pub refund_id: RefundId,
}

/// Ask the provider what happened to an unresolved refund. Never refunds
/// again (ADR 018, applied to refunds).
#[derive(Debug, Clone)]
pub struct ReconcileRefundCommand {
    pub request_id: Option<Uuid>,
    pub refund_id: RefundId,
}

/// Record that a sale was invoiced upstream.
///
/// **Test and development only**, behind the `testing` feature. PI.Edge is not
/// the authority for declaring a sale invoiced: it *consumes* fiscal metadata
/// that an upstream system owns, and has no production operation to write it.
/// Until a sync brings that metadata in, this is how a fixture arranges an
/// invoiced sale (ADR 032).
#[cfg(feature = "testing")]
#[derive(Debug, Clone)]
pub struct MarkTransactionInvoicedCommand {
    pub request_id: Option<Uuid>,
    pub transaction_id: TransactionId,
    /// Canonical, opaque reference assigned upstream.
    pub invoice_ref: String,
}

// ---------------------------------------------------------------- hardware

/// A physical scan, ready to become an item.
///
/// The host captured the keystrokes and `edge-hardware`'s parser decided a scan
/// completed; this is the result crossing into the application (ADR 036).
#[derive(Debug, Clone)]
pub struct SubmitScanCommand {
    pub scan: ScanEvent,
}

/// Print a finalized sale.
#[derive(Debug, Clone)]
pub struct PrintTransactionCommand {
    pub request_id: Option<Uuid>,
    pub transaction_id: TransactionId,
    /// A reprint puts paper out and changes nothing else — no cash, no stock,
    /// no drawer, no sale (ADR 037).
    pub reprint: bool,
}

/// Open the drawer because an operator asked, from the diagnostic surface.
///
/// Unlike a drawer opening caused by a payment, this is not keyed to any
/// commercial fact: it opens every time it is asked, and every opening is
/// audited (ADR 037).
#[derive(Debug, Clone)]
pub struct OpenDrawerCommand {
    pub request_id: Option<Uuid>,
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
    /// Promotions for the products in this chunk, for the same reason as the
    /// prices: a promotion that names a product must land with it.
    pub promotions: Vec<Promotion>,
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

/// Start a payment for the active sale.
///
/// Cash settles inside this command. A provider-backed tender records the
/// payment and its first attempt durably, calls the provider, and records the
/// result in a second transaction (ADR 020).
#[derive(Debug, Clone)]
pub struct StartPaymentCommand {
    pub request_id: Option<Uuid>,
    pub tender_type: TenderType,
    /// What this tender is asked to cover, in minor units. For cash, what the
    /// customer handed over.
    pub amount_minor: i64,
}

/// Ask the provider what happened to an unresolved attempt. Never charges.
#[derive(Debug, Clone)]
pub struct ReconcilePaymentCommand {
    pub request_id: Option<Uuid>,
    pub payment_id: PaymentId,
}

/// Undo a payment that must not remain applied (ADR 019).
#[derive(Debug, Clone)]
pub struct ReversePaymentCommand {
    pub request_id: Option<Uuid>,
    pub payment_id: PaymentId,
}

/// Abandon a sale that has taken no money.
#[derive(Debug, Clone, Default)]
pub struct CancelTransactionCommand {
    pub request_id: Option<Uuid>,
    /// Why, for the journal. Optional: "the customer changed their mind" is
    /// the common case and does not need a code.
    pub reason_code: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FinalizeTransactionCommand {
    pub request_id: Option<Uuid>,
}

/// Put money into the drawer for an operational reason.
#[derive(Debug, Clone)]
pub struct CashInCommand {
    pub request_id: Option<Uuid>,
    pub amount_minor: i64,
    /// Why. A blank reason is refused: an unexplained drawer movement is not
    /// an audit trail.
    pub reason_code: String,
}

/// Take money out of the drawer for an operational reason.
#[derive(Debug, Clone)]
pub struct CashOutCommand {
    pub request_id: Option<Uuid>,
    pub amount_minor: i64,
    pub reason_code: String,
}

/// Ask for money to be moved to the safe. Changes no cash on its own.
#[derive(Debug, Clone)]
pub struct RequestSafeDropCommand {
    pub request_id: Option<Uuid>,
    pub amount_minor: i64,
    pub reason_code: String,
}

/// Authorise a pending drop. `approver` must not be the requester (ADR 023).
#[derive(Debug, Clone)]
pub struct ApproveSafeDropCommand {
    pub request_id: Option<Uuid>,
    pub safe_drop_id: Uuid,
    /// An identity the application layer has already authorised. The domain
    /// does not evaluate roles.
    pub approver: OperatorId,
    pub approve: bool,
}

/// Begin a close. The answer says what is still needed and deliberately does
/// not say what the drawer should contain (ADR 022).
#[derive(Debug, Clone, Default)]
pub struct RequestSessionCloseCommand {
    pub request_id: Option<Uuid>,
}

/// Declare the counted cash. Only after this is durable does the core reveal
/// the expected figure and the variance.
#[derive(Debug, Clone)]
pub struct DeclareClosingCashCommand {
    pub request_id: Option<Uuid>,
    pub counted_minor: i64,
}

/// Continue a closed session. Creates a new session linked to it; the closed
/// one keeps its declaration and variance (ADR 023).
#[derive(Debug, Clone)]
pub struct ReopenSessionCommand {
    pub request_id: Option<Uuid>,
    pub session_id: SessionId,
    pub reason: String,
    /// Who takes over. May differ from the previous operator.
    pub operator_id: OperatorId,
}

/// End the day over open sessions, under an explicit policy (ADR 024).
#[derive(Debug, Clone)]
pub struct ForceCloseBusinessDayCommand {
    pub request_id: Option<Uuid>,
    pub reason: String,
    /// Who or what ordered it: an operator, a console, later a scheduler.
    pub actor: String,
}

#[derive(Debug, Clone, Default)]
pub struct CloseRegisterSessionCommand {
    pub request_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct CloseBusinessDayCommand {
    pub request_id: Option<Uuid>,
}
