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

    #[error("selection group `{group_id}` appears twice in this snapshot")]
    DuplicateSelectionGroup { group_id: String },

    #[error("selection option `{option_id}` appears twice in this snapshot")]
    DuplicateSelectionOption { option_id: String },

    #[error("selection group `{group_id}` is not in this snapshot")]
    UnknownSelectionGroup { group_id: String },

    #[error("selection group `{group_id}` hangs from `{owner}`, which is not in this snapshot")]
    UnknownSelectionOwner { group_id: String, owner: String },

    /// A chargeable option whose component is absent: the price books would
    /// have nothing to price it from.
    #[error(
        "selection option `{option_id}` charges for `{product_ref}`, \
         which is not in this snapshot"
    )]
    UnknownSelectionComponent {
        option_id: String,
        product_ref: String,
    },

    #[error("selection group `{group_id}` is part of a cycle of groups")]
    CyclicSelectionGroup { group_id: String },

    #[error("selection group `{group_id}` has impossible defaults: {reason}")]
    InvalidSelectionDefaults { group_id: String, reason: String },

    #[error("channel `{channel_ref}` appears twice in this snapshot")]
    DuplicateChannel { channel_ref: String },

    #[error("channel `{channel_ref}` is not declared in this snapshot")]
    UnknownChannel { channel_ref: String },

    #[error("channel `{channel_ref}` says two things about `{target}`")]
    DuplicateAvailability { channel_ref: String, target: String },

    #[error("category `{category_ref}` appears twice in this snapshot")]
    DuplicateCategory { category_ref: String },

    #[error(
        "`{product_ref}` is shown in category `{category_ref}`, \
         which is not in this snapshot"
    )]
    UnknownCategory {
        product_ref: String,
        category_ref: String,
    },

    #[error("`{product_ref}` has two presentation records in this snapshot")]
    DuplicatePresentation { product_ref: String },

    #[error("`{product_ref}` has a presentation but is not in this snapshot")]
    PresentationWithoutProduct { product_ref: String },

    #[error("availability names `{target}`, which is not in this snapshot")]
    UnknownAvailabilityTarget { target: String },

    /// A client asked for a choice this product does not offer — or asked for
    /// a nested one without choosing the answer it hangs from.
    #[error("`{option_id}` is not a choice this product offers")]
    OptionNotOffered { option_id: String },

    /// More of something than it allows, whether that is one option or the
    /// whole group.
    #[error("`{reference}` accepts at most {limit}, not {requested}")]
    SelectionQuantityRefused {
        reference: String,
        limit: i64,
        requested: i64,
    },

    /// A question that has to be answered was not.
    #[error("`{name}` needs at least {minimum} chosen")]
    RequiredSelectionMissing {
        group_id: String,
        name: String,
        minimum: i64,
    },

    /// The same bytes declared two ways. A content hash identifies exactly
    /// one sequence of bytes, so it has exactly one length and one media type
    /// (ADR 045).
    #[error("asset `{content_hash}` is declared with two different sizes or media types")]
    ConflictingAssetDeclaration { content_hash: String },

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
            CatalogError::DuplicateSelectionGroup { .. } => "CATALOG_DUPLICATE_SELECTION_GROUP",
            CatalogError::DuplicateSelectionOption { .. } => "CATALOG_DUPLICATE_SELECTION_OPTION",
            CatalogError::UnknownSelectionGroup { .. } => "CATALOG_UNKNOWN_SELECTION_GROUP",
            CatalogError::UnknownSelectionOwner { .. } => "CATALOG_UNKNOWN_SELECTION_OWNER",
            CatalogError::UnknownSelectionComponent { .. } => "CATALOG_UNKNOWN_SELECTION_COMPONENT",
            CatalogError::CyclicSelectionGroup { .. } => "CATALOG_CYCLIC_SELECTION_GROUP",
            CatalogError::InvalidSelectionDefaults { .. } => "CATALOG_INVALID_SELECTION_DEFAULTS",
            CatalogError::DuplicateChannel { .. } => "CATALOG_DUPLICATE_CHANNEL",
            CatalogError::UnknownChannel { .. } => "CATALOG_UNKNOWN_CHANNEL",
            CatalogError::DuplicateAvailability { .. } => "CATALOG_DUPLICATE_AVAILABILITY",
            CatalogError::DuplicateCategory { .. } => "CATALOG_DUPLICATE_CATEGORY",
            CatalogError::UnknownCategory { .. } => "CATALOG_UNKNOWN_CATEGORY",
            CatalogError::DuplicatePresentation { .. } => "CATALOG_DUPLICATE_PRESENTATION",
            CatalogError::PresentationWithoutProduct { .. } => {
                "CATALOG_PRESENTATION_WITHOUT_PRODUCT"
            }
            CatalogError::UnknownAvailabilityTarget { .. } => "CATALOG_UNKNOWN_AVAILABILITY_TARGET",
            CatalogError::OptionNotOffered { .. } => "CATALOG_OPTION_NOT_OFFERED",
            CatalogError::SelectionQuantityRefused { .. } => "CATALOG_SELECTION_QUANTITY_REFUSED",
            CatalogError::RequiredSelectionMissing { .. } => "CATALOG_SELECTION_REQUIRED",
            CatalogError::ConflictingAssetDeclaration { .. } => {
                "CATALOG_CONFLICTING_ASSET_DECLARATION"
            }
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
