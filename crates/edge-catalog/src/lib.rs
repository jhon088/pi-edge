//! The local catalog bounded context.
//!
//! **PI.Edge is not the master of products.** Product data originates in
//! ERP/master systems, is consolidated by PI.COMMERCE Server, and arrives here
//! as a versioned, immutable *snapshot*. This crate owns the local projection
//! of that snapshot and the resolution of a scanned code into a priced,
//! taxed sale line (see ADR 011, ADR 012).
//!
//! Like [`edge_domain`], this crate is pure: no SQLite, no HTTP, no transport,
//! no clock. Persistence lives in `edge-storage`, orchestration in
//! `edge-application`.
//!
//! ```text
//! barcode / sku / product_ref
//!      -> CatalogProduct + TaxProfile   (from one snapshot, never "the latest")
//!      -> resolve_price                 (price books, scope + priority + validity)
//!      -> ResolvedSaleLine              (unit price, tax rate, price mode)
//!      -> edge-domain computes the line and the totals
//! ```

pub mod content;
pub mod error;
pub mod pricing;
pub mod product;
pub mod resolution;
pub mod snapshot;
pub mod tax;

pub use content::{hash_chunk, hash_snapshot, ContentHasher, SnapshotContent};
pub use error::CatalogError;
pub use pricing::{
    resolve_price, NewPriceBook, NewPriceEntry, PriceBook, PriceBookScope, PriceCandidate,
    PriceEntry, PricingContext, ResolvedPrice,
};
pub use product::{Barcode, CatalogBarcode, CatalogProduct, ProductSelector};
pub use resolution::{resolve_sale_line, ResolvedSaleLine};
pub use snapshot::{
    CatalogSnapshot, CatalogSnapshotStatus, ImportProgress, NewCatalogSnapshot, SnapshotCounts,
};
pub use tax::{TaxProfile, TaxProfileCode};

/// Re-exported so a producer of snapshots (the server, the CLI importer) can
/// speak the content model without depending on the sale domain directly.
pub use edge_domain::prelude::{
    Currency, Money, PriceBookId, PriceMode, ProductRef, Quantity, StoreId, TaxRate, Timestamp,
};

/// Version of the catalog model, reported in diagnostics.
pub const CATALOG_MODEL_VERSION: u32 = 1;
