//! Conversions for the catalog content model.
//!
//! Enabled by the `catalog-mapping` feature. These convert between the wire
//! records and the pure value objects of `edge-catalog`/`edge-domain`, with no
//! application layer involved, so a *producer* of snapshots can build and hash
//! exactly what PI.Edge will verify.

use edge_catalog::pricing::{NewPriceBook, NewPriceEntry, PriceBookScope};
use edge_catalog::product::NewCatalogProduct;
use edge_catalog::{
    Barcode, CatalogBarcode, CatalogError, CatalogProduct, CatalogSnapshot, ImportProgress,
    PriceBook, PriceEntry, ProductSelector, ResolvedPrice, SnapshotCounts, TaxProfile,
    TaxProfileCode,
};
use edge_domain::prelude::*;

use crate::dto::*;

impl From<&Money> for MoneyV1 {
    fn from(value: &Money) -> Self {
        Self {
            minor: value.minor(),
            currency: value.currency().code().to_owned(),
            formatted: value.format_amount(),
        }
    }
}

impl ProductSelectorV1 {
    pub fn into_domain(self) -> Result<ProductSelector, CatalogError> {
        Ok(match self {
            ProductSelectorV1::Barcode(value) => ProductSelector::Barcode(Barcode::new(value)?),
            ProductSelectorV1::Sku(value) => {
                let trimmed = value.trim().to_owned();
                if trimmed.is_empty() {
                    return Err(CatalogError::InvalidRecord {
                        field: "sku",
                        reason: "must not be empty".to_owned(),
                    });
                }
                ProductSelector::Sku(trimmed)
            }
            ProductSelectorV1::ProductRef(value) => {
                ProductSelector::ProductRef(ProductRef::new(value)?)
            }
        })
    }
}

impl From<&ProductSelector> for ProductSelectorV1 {
    fn from(selector: &ProductSelector) -> Self {
        match selector {
            ProductSelector::Barcode(value) => {
                ProductSelectorV1::Barcode(value.as_str().to_owned())
            }
            ProductSelector::Sku(value) => ProductSelectorV1::Sku(value.clone()),
            ProductSelector::ProductRef(value) => {
                ProductSelectorV1::ProductRef(value.as_str().to_owned())
            }
        }
    }
}

impl From<SnapshotCounts> for SnapshotCountsV1 {
    fn from(counts: SnapshotCounts) -> Self {
        Self {
            tax_profiles: counts.tax_profiles,
            price_books: counts.price_books,
            products: counts.products,
            price_entries: counts.price_entries,
            barcodes: counts.barcodes,
        }
    }
}

impl From<SnapshotCountsV1> for SnapshotCounts {
    fn from(counts: SnapshotCountsV1) -> Self {
        Self {
            tax_profiles: counts.tax_profiles,
            price_books: counts.price_books,
            products: counts.products,
            price_entries: counts.price_entries,
            barcodes: counts.barcodes,
        }
    }
}

impl From<&PriceBook> for PriceBookV1 {
    fn from(book: &PriceBook) -> Self {
        Self {
            price_book_id: book.price_book_id.to_string(),
            code: book.code.clone(),
            name: book.name.clone(),
            currency: book.currency.code().to_owned(),
            scope: book.scope.as_str().to_owned(),
            store_id: book.scope.store_id().map(ToString::to_string),
            priority: book.priority,
            active: book.active,
        }
    }
}

impl PriceBookV1 {
    pub fn into_domain(self) -> Result<PriceBook, CatalogError> {
        let scope = match (self.scope.as_str(), self.store_id) {
            ("TENANT", None) => PriceBookScope::Tenant,
            ("TENANT", Some(_)) => {
                return Err(CatalogError::InvalidRecord {
                    field: "price_book_scope",
                    reason: "a tenant book must not name a store".to_owned(),
                })
            }
            ("STORE", Some(store_id)) => PriceBookScope::Store {
                store_id: StoreId::new(store_id)?,
            },
            ("STORE", None) => {
                return Err(CatalogError::InvalidRecord {
                    field: "price_book_scope",
                    reason: "a store book must name its store".to_owned(),
                })
            }
            (other, _) => {
                return Err(CatalogError::InvalidRecord {
                    field: "price_book_scope",
                    reason: format!("unknown scope `{other}`"),
                })
            }
        };
        PriceBook::import(NewPriceBook {
            price_book_id: PriceBookId::new(self.price_book_id)?,
            code: self.code,
            name: self.name,
            currency: Currency::from_code(&self.currency)?,
            scope,
            priority: self.priority,
            active: self.active,
        })
    }
}

impl From<&PriceEntry> for PriceEntryV1 {
    fn from(entry: &PriceEntry) -> Self {
        Self {
            price_book_id: entry.price_book_id.to_string(),
            product_ref: entry.product_ref.to_string(),
            amount: MoneyV1::from(&entry.amount),
            price_mode: entry.price_mode.as_str().to_owned(),
            valid_from: entry.valid_from.map(|ts| ts.to_rfc3339()),
            valid_until: entry.valid_until.map(|ts| ts.to_rfc3339()),
        }
    }
}

impl From<&ResolvedPrice> for ResolvedPriceV1 {
    fn from(price: &ResolvedPrice) -> Self {
        Self {
            price_book_id: price.price_book_id.to_string(),
            price_book_code: price.price_book_code.clone(),
            unit_price: MoneyV1::from(&price.unit_price),
            price_mode: price.price_mode.as_str().to_owned(),
            effective_at: price.effective_at.to_rfc3339(),
        }
    }
}

impl PriceRecordV1 {
    /// Validate one price for a product.
    pub fn into_domain(
        self,
        product_ref: &ProductRef,
        currency: Currency,
    ) -> Result<PriceEntry, CatalogError> {
        let valid_from = match self.valid_from.as_deref() {
            Some(value) => Some(parse_timestamp(value, "valid_from")?),
            None => None,
        };
        let valid_until = match self.valid_until.as_deref() {
            Some(value) => Some(parse_timestamp(value, "valid_until")?),
            None => None,
        };
        PriceEntry::import(NewPriceEntry {
            price_book_id: PriceBookId::new(self.price_book)?,
            product_ref: product_ref.clone(),
            amount: Money::from_minor(self.amount_minor, currency),
            price_mode: PriceMode::parse(&self.price_mode)?,
            valid_from,
            valid_until,
        })
    }
}

impl From<&TaxProfile> for TaxProfileV1 {
    fn from(profile: &TaxProfile) -> Self {
        Self {
            code: profile.code.as_str().to_owned(),
            name: profile.name.clone(),
            rate_bps: profile.rate.bps(),
        }
    }
}

impl From<&CatalogProduct> for CatalogProductV1 {
    fn from(product: &CatalogProduct) -> Self {
        Self {
            product_ref: product.product_ref.to_string(),
            external_ref: product.external_ref.clone(),
            sku: product.sku.clone(),
            name: product.name.clone(),
            active: product.active,
            sellable: product.sellable,
            tax_profile: product.tax_profile.to_string(),
        }
    }
}

impl From<&CatalogSnapshot> for CatalogSnapshotV1 {
    fn from(snapshot: &CatalogSnapshot) -> Self {
        let state = snapshot.state();
        Self {
            snapshot_id: state.snapshot_id.to_string(),
            store_id: state.store_id.to_string(),
            version: state.version,
            status: state.status.as_str().to_owned(),
            declared_content_hash: state.declared_content_hash.clone(),
            content_hash: state.content_hash.clone(),
            generated_at: state.generated_at.to_rfc3339(),
            effective_at: state.effective_at.map(|ts| ts.to_rfc3339()),
            expected: SnapshotCountsV1::from(state.expected),
            imported_at: state.imported_at.to_rfc3339(),
            committed_at: state.committed_at.map(|ts| ts.to_rfc3339()),
            activated_at: state.activated_at.map(|ts| ts.to_rfc3339()),
            superseded_at: state.superseded_at.map(|ts| ts.to_rfc3339()),
        }
    }
}

impl From<&ImportProgress> for ImportProgressV1 {
    fn from(progress: &ImportProgress) -> Self {
        Self {
            snapshot_id: progress.snapshot_id.to_string(),
            status: progress.status.as_str().to_owned(),
            version: progress.version,
            received_chunks: progress.received_chunks.clone(),
            received_records: progress.received_records,
            next_chunk_index: progress.next_chunk_index(),
            expected: SnapshotCountsV1::from(progress.expected),
            stored: SnapshotCountsV1::from(progress.stored),
        }
    }
}

impl TaxProfileV1 {
    pub fn into_domain(self) -> Result<TaxProfile, CatalogError> {
        TaxProfile::new(
            TaxProfileCode::new(self.code)?,
            self.name,
            TaxRate::from_bps(self.rate_bps)?,
        )
    }
}

impl CatalogProductRecordV1 {
    /// Validate one record into a product plus its barcodes.
    ///
    /// Currency comes from the device: a snapshot belongs to one store, and the
    /// store's currency is not a per-record choice.
    pub fn into_domain(self, currency: Currency) -> Result<ProductRecord, CatalogError> {
        let product_ref = ProductRef::new(self.product_ref)?;
        let product = CatalogProduct::import(NewCatalogProduct {
            product_ref: product_ref.clone(),
            external_ref: self.external_ref,
            sku: self.sku,
            name: self.name,
            active: self.active,
            sellable: self.sellable,
            tax_profile: TaxProfileCode::new(self.tax_profile)?,
        })?;
        let barcodes = self
            .barcodes
            .into_iter()
            .map(|code| {
                Ok(CatalogBarcode {
                    barcode: Barcode::new(code)?,
                    product_ref: product_ref.clone(),
                })
            })
            .collect::<Result<Vec<_>, CatalogError>>()?;
        let prices = self
            .prices
            .into_iter()
            .map(|price| price.into_domain(&product_ref, currency))
            .collect::<Result<Vec<_>, CatalogError>>()?;
        Ok(ProductRecord {
            product,
            prices,
            barcodes,
        })
    }
}

pub(crate) fn parse_snapshot_id(value: &str) -> Result<CatalogSnapshotId, CatalogError> {
    value.parse::<CatalogSnapshotId>().map_err(CatalogError::Id)
}

pub(crate) fn parse_timestamp(value: &str, field: &'static str) -> Result<Timestamp, CatalogError> {
    Timestamp::parse_rfc3339(value).map_err(|_| CatalogError::InvalidRecord {
        field,
        reason: format!("`{value}` is not an RFC-3339 UTC timestamp"),
    })
}

impl CatalogSnapshotRequestV1 {
    pub fn snapshot_id(&self) -> Result<CatalogSnapshotId, CatalogError> {
        parse_snapshot_id(&self.snapshot_id)
    }
}

/// One validated product record: the product itself plus the prices and codes
/// that travel with it.
#[derive(Debug, Clone)]
pub struct ProductRecord {
    pub product: CatalogProduct,
    pub prices: Vec<PriceEntry>,
    pub barcodes: Vec<CatalogBarcode>,
}
