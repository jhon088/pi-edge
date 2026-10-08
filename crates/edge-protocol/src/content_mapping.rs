//! Conversions for the catalog content model.
//!
//! Enabled by the `catalog-mapping` feature. These convert between the wire
//! records and the pure value objects of `edge-catalog`/`edge-domain`, with no
//! application layer involved, so a *producer* of snapshots can build and hash
//! exactly what PI.Edge will verify.

use edge_catalog::product::NewCatalogProduct;
use edge_catalog::{
    Barcode, CatalogBarcode, CatalogError, CatalogProduct, CatalogSnapshot, ImportProgress,
    ProductSelector, SnapshotCounts, TaxProfile, TaxProfileCode,
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
            products: counts.products,
            barcodes: counts.barcodes,
        }
    }
}

impl From<&TaxProfile> for TaxProfileV1 {
    fn from(profile: &TaxProfile) -> Self {
        Self {
            code: profile.code.as_str().to_owned(),
            name: profile.name.clone(),
            rate_bps: profile.rate.bps(),
            price_mode: profile.price_mode.as_str().to_owned(),
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
            unit_price: MoneyV1::from(&product.unit_price),
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
            PriceMode::parse(&self.price_mode)?,
        )
    }
}

impl CatalogProductRecordV1 {
    /// Validate one record into a product plus its barcodes.
    ///
    /// Currency comes from the device: a snapshot belongs to one store, and the
    /// store's currency is not a per-record choice.
    pub fn into_domain(
        self,
        currency: Currency,
    ) -> Result<(CatalogProduct, Vec<CatalogBarcode>), CatalogError> {
        let product_ref = ProductRef::new(self.product_ref)?;
        let product = CatalogProduct::import(NewCatalogProduct {
            product_ref: product_ref.clone(),
            external_ref: self.external_ref,
            sku: self.sku,
            name: self.name,
            active: self.active,
            sellable: self.sellable,
            tax_profile: TaxProfileCode::new(self.tax_profile)?,
            unit_price: Money::from_minor(self.unit_price_minor, currency),
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
        Ok((product, barcodes))
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
