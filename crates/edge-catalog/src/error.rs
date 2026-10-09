//! Typed catalog errors.
//!
//! Each variant carries a stable code that the protocol layer publishes
//! verbatim, so a client can branch on "unknown barcode" versus "product not
//! sellable" without parsing messages.

use edge_domain::prelude::{IdError, MoneyError};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    #[error(transparent)]
    Money(#[from] MoneyError),
    #[error(transparent)]
    Id(#[from] IdError),

    #[error("no product matches {selector}")]
    ProductNotFound { selector: String },
    #[error("product {product_ref} is not active in this catalog")]
    ProductInactive { product_ref: String },
    #[error("product {product_ref} is not sellable")]
    ProductNotSellable { product_ref: String },
    #[error("product {product_ref} references unknown tax profile `{code}`")]
    UnknownTaxProfile { product_ref: String, code: String },

    #[error("barcode `{barcode}` appears twice in this snapshot")]
    DuplicateBarcode { barcode: String },
    #[error("product `{product_ref}` appears twice in this snapshot")]
    DuplicateProduct { product_ref: String },
    #[error("invalid catalog record: {field} {reason}")]
    InvalidRecord { field: &'static str, reason: String },
    #[error("catalog price is in {actual}, but this store sells in {expected}")]
    CurrencyMismatch {
        expected: &'static str,
        actual: &'static str,
    },

    #[error("no price for product {product_ref} at store {store_id} effective {effective_at}")]
    PriceNotFound {
        product_ref: String,
        store_id: String,
        effective_at: String,
    },
    #[error(
        "price for {product_ref} is ambiguous: price books [{price_books}] all apply at \
         scope {scope} priority {priority}"
    )]
    AmbiguousPrice {
        product_ref: String,
        scope: &'static str,
        priority: i32,
        price_books: String,
    },
    #[error("price entry references unknown price book `{price_book_id}`")]
    UnknownPriceBook { price_book_id: String },
    #[error("price book `{price_book_id}` has two prices for {product_ref} starting at the same instant")]
    DuplicatePriceEntry {
        price_book_id: String,
        product_ref: String,
    },
    #[error(
        "invalid validity window: valid_until {valid_until} is not after valid_from {valid_from}"
    )]
    InvalidValidity {
        valid_from: String,
        valid_until: String,
    },

    #[error(
        "promotions [{promotions}] are all exclusive at priority {priority} for {target}: \
         refusing to choose"
    )]
    AmbiguousPromotion {
        target: String,
        priority: i32,
        promotions: String,
    },
    #[error("promotion `{promotion_id}` is not declared by this snapshot")]
    UnknownPromotion { promotion_id: String },

    #[error("chunk {index} was already stored with different content")]
    ChunkConflict { index: i64 },
    #[error("chunk {index} is missing")]
    MissingChunk { index: i64 },
    #[error("expected {expected} {what}, stored {stored}")]
    CountMismatch {
        what: &'static str,
        expected: i64,
        stored: i64,
    },
    #[error("content hash mismatch: client declared {declared}, core computed {computed}")]
    ContentHashMismatch { declared: String, computed: String },

    #[error("catalog snapshot {snapshot_id} not found")]
    SnapshotNotFound { snapshot_id: String },
    #[error("catalog import {snapshot_id} is {status}, not IMPORTING")]
    ImportNotOpen {
        snapshot_id: String,
        status: &'static str,
    },
    #[error("catalog snapshot {snapshot_id} is {status}, not PENDING")]
    SnapshotNotPending {
        snapshot_id: String,
        status: &'static str,
    },
    #[error("this store has no active catalog snapshot")]
    NoActiveSnapshot,
    #[error("catalog snapshot version {version} already exists with different content")]
    VersionConflict { version: i64 },
    #[error("invalid catalog snapshot transition: {from} -> {attempted}")]
    InvalidTransition {
        from: &'static str,
        attempted: &'static str,
    },
}

impl CatalogError {
    /// Stable code published through the Edge Protocol.
    pub fn code(&self) -> &'static str {
        match self {
            CatalogError::Money(_) | CatalogError::Id(_) => "INVALID_ARGUMENT",
            CatalogError::ProductNotFound { .. } => "PRODUCT_NOT_FOUND",
            CatalogError::ProductInactive { .. } => "PRODUCT_INACTIVE",
            CatalogError::ProductNotSellable { .. } => "PRODUCT_NOT_SELLABLE",
            CatalogError::UnknownTaxProfile { .. } => "CATALOG_UNKNOWN_TAX_PROFILE",
            CatalogError::DuplicateBarcode { .. } => "CATALOG_DUPLICATE_BARCODE",
            CatalogError::DuplicateProduct { .. } => "CATALOG_DUPLICATE_PRODUCT",
            CatalogError::InvalidRecord { .. } => "CATALOG_INVALID_RECORD",
            CatalogError::CurrencyMismatch { .. } => "CATALOG_CURRENCY_MISMATCH",
            CatalogError::PriceNotFound { .. } => "PRICE_NOT_FOUND",
            CatalogError::AmbiguousPrice { .. } => "AMBIGUOUS_PRICE",
            CatalogError::UnknownPriceBook { .. } => "CATALOG_UNKNOWN_PRICE_BOOK",
            CatalogError::DuplicatePriceEntry { .. } => "CATALOG_DUPLICATE_PRICE_ENTRY",
            CatalogError::InvalidValidity { .. } => "CATALOG_INVALID_VALIDITY",
            CatalogError::AmbiguousPromotion { .. } => "AMBIGUOUS_PROMOTION",
            CatalogError::UnknownPromotion { .. } => "CATALOG_UNKNOWN_PROMOTION",
            CatalogError::ChunkConflict { .. } => "CATALOG_CHUNK_CONFLICT",
            CatalogError::MissingChunk { .. } => "CATALOG_IMPORT_INCOMPLETE",
            CatalogError::CountMismatch { .. } => "CATALOG_COUNT_MISMATCH",
            CatalogError::ContentHashMismatch { .. } => "CATALOG_HASH_MISMATCH",
            CatalogError::SnapshotNotFound { .. } => "CATALOG_SNAPSHOT_NOT_FOUND",
            CatalogError::ImportNotOpen { .. } => "CATALOG_IMPORT_NOT_OPEN",
            CatalogError::SnapshotNotPending { .. } => "CATALOG_SNAPSHOT_NOT_PENDING",
            CatalogError::NoActiveSnapshot => "NO_ACTIVE_CATALOG",
            CatalogError::VersionConflict { .. } => "CATALOG_VERSION_CONFLICT",
            CatalogError::InvalidTransition { .. } => "CATALOG_INVALID_TRANSITION",
        }
    }
}
