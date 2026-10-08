//! Deterministic ports for tests and golden cases.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use chrono::{DateTime, Duration, Utc};
use edge_domain::prelude::Timestamp;
use uuid::Uuid;

use crate::ports::{Clock, IdFactory};

/// Clock that starts at a fixed instant and advances by `step` on every read.
/// A `step` of zero freezes time, which is what golden cases use.
#[derive(Debug)]
pub struct FixedClock {
    start: DateTime<Utc>,
    step_millis: i64,
    reads: AtomicI64,
}

impl FixedClock {
    pub fn new(start: DateTime<Utc>, step_millis: i64) -> Self {
        Self {
            start,
            step_millis,
            reads: AtomicI64::new(0),
        }
    }

    pub fn frozen(start: DateTime<Utc>) -> Self {
        Self::new(start, 0)
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        let reads = self.reads.fetch_add(1, Ordering::SeqCst);
        let offset = Duration::milliseconds(self.step_millis.saturating_mul(reads));
        Timestamp::from_datetime(self.start + offset)
    }
}

/// Sequential UUID factory: `00000000-0000-0000-0000-0000000000NN`.
#[derive(Debug, Default)]
pub struct SequentialIdFactory {
    next: AtomicU64,
}

impl SequentialIdFactory {
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
        }
    }

    pub fn starting_at(value: u64) -> Self {
        Self {
            next: AtomicU64::new(value),
        }
    }
}

impl IdFactory for SequentialIdFactory {
    fn new_uuid(&self) -> Uuid {
        Uuid::from_u128(u128::from(self.next.fetch_add(1, Ordering::SeqCst)))
    }
}

// ----------------------------------------------------------------- catalog

use edge_catalog::product::NewCatalogProduct;
use edge_catalog::{
    hash_chunk, hash_snapshot, Barcode, CatalogBarcode, CatalogProduct, Currency, Money, PriceMode,
    ProductRef, TaxProfile, TaxProfileCode, TaxRate,
};
use edge_domain::prelude::CatalogSnapshotId;

use crate::commands::{
    ActivateCatalogSnapshotCommand, BeginCatalogImportCommand, CommitCatalogImportCommand,
    PutCatalogChunkCommand,
};
use crate::error::AppError;
use crate::ports::{EdgeStore, IdFactory as IdFactoryPort};
use crate::queries::CatalogSnapshotView;
use crate::service::EdgeApplication;

/// One product in a test snapshot.
#[derive(Debug, Clone)]
pub struct FixtureProduct {
    pub product_ref: &'static str,
    pub name: &'static str,
    pub sku: Option<&'static str>,
    pub barcodes: Vec<&'static str>,
    pub price_minor: i64,
    pub tax_profile: &'static str,
    pub active: bool,
    pub sellable: bool,
}

impl FixtureProduct {
    /// A sellable product at 16% tax-exclusive.
    pub fn new(product_ref: &'static str, name: &'static str, price_minor: i64) -> Self {
        Self {
            product_ref,
            name,
            sku: None,
            barcodes: Vec::new(),
            price_minor,
            tax_profile: "IVA16",
            active: true,
            sellable: true,
        }
    }

    pub fn sku(mut self, sku: &'static str) -> Self {
        self.sku = Some(sku);
        self
    }

    pub fn barcode(mut self, barcode: &'static str) -> Self {
        self.barcodes.push(barcode);
        self
    }

    pub fn tax_profile(mut self, code: &'static str) -> Self {
        self.tax_profile = code;
        self
    }

    pub fn inactive(mut self) -> Self {
        self.active = false;
        self
    }

    pub fn not_sellable(mut self) -> Self {
        self.sellable = false;
        self
    }
}

/// Builds and imports catalog snapshots through the real use cases, so tests
/// exercise the same path the CLI and the server do.
#[derive(Debug, Clone)]
pub struct CatalogFixture {
    pub version: i64,
    pub currency: Currency,
    pub generated_at: Timestamp,
    pub profiles: Vec<TaxProfile>,
    pub products: Vec<FixtureProduct>,
}

impl CatalogFixture {
    /// A snapshot pre-loaded with the Mexican rates we care about, in both
    /// price modes.
    pub fn new(version: i64) -> Self {
        let profile = |code: &str, bps: u32, mode: PriceMode| {
            #[allow(clippy::expect_used)]
            TaxProfile::new(
                TaxProfileCode::new(code).expect("valid code"),
                format!("profile {code}"),
                TaxRate::from_bps(bps).expect("valid rate"),
                mode,
            )
            .expect("valid profile")
        };
        Self {
            version,
            currency: Currency::Mxn,
            #[allow(clippy::expect_used)]
            generated_at: Timestamp::parse_rfc3339("2026-10-01T00:00:00Z")
                .expect("valid timestamp"),
            profiles: vec![
                profile("IVA16", 1_600, PriceMode::TaxExclusive),
                profile("IVA8", 800, PriceMode::TaxExclusive),
                profile("IVA0", 0, PriceMode::TaxExclusive),
                profile("IVA16-INC", 1_600, PriceMode::TaxInclusive),
                profile("IVA8-INC", 800, PriceMode::TaxInclusive),
                profile("IVA0-INC", 0, PriceMode::TaxInclusive),
            ],
            products: Vec::new(),
        }
    }

    /// The default single-product catalog used by the sale tests: 100.00 MXN
    /// at 16%, tax exclusive.
    pub fn demo(version: i64) -> Self {
        Self::new(version).with(
            FixtureProduct::new("SKU-001", "Producto prueba", 10_000)
                .sku("SKU-001")
                .barcode(DEMO_BARCODE),
        )
    }

    pub fn with(mut self, product: FixtureProduct) -> Self {
        self.products.push(product);
        self
    }

    fn records(&self) -> (Vec<CatalogProduct>, Vec<CatalogBarcode>) {
        let mut products = Vec::new();
        let mut barcodes = Vec::new();
        for fixture in &self.products {
            #[allow(clippy::expect_used)]
            let product_ref = ProductRef::new(fixture.product_ref).expect("valid product ref");
            #[allow(clippy::expect_used)]
            let product = CatalogProduct::import(NewCatalogProduct {
                product_ref: product_ref.clone(),
                external_ref: None,
                sku: fixture.sku.map(str::to_owned),
                name: fixture.name.to_owned(),
                active: fixture.active,
                sellable: fixture.sellable,
                tax_profile: TaxProfileCode::new(fixture.tax_profile).expect("valid code"),
                unit_price: Money::from_minor(fixture.price_minor, self.currency),
            })
            .expect("valid product");
            for code in &fixture.barcodes {
                #[allow(clippy::expect_used)]
                barcodes.push(CatalogBarcode {
                    barcode: Barcode::new(*code).expect("valid barcode"),
                    product_ref: product_ref.clone(),
                });
            }
            products.push(product);
        }
        (products, barcodes)
    }

    /// Import the snapshot and leave it `PENDING`.
    pub fn import<S, C, I>(
        &self,
        app: &EdgeApplication<S, C, I>,
    ) -> Result<CatalogSnapshotView, AppError>
    where
        S: EdgeStore,
        C: Clock,
        I: IdFactoryPort,
    {
        let (products, barcodes) = self.records();
        let opened = app.begin_catalog_import(BeginCatalogImportCommand {
            request_id: None,
            version: self.version,
            currency: self.currency,
            declared_content_hash: hash_snapshot(&self.profiles, &products, &barcodes),
            generated_at: self.generated_at,
            effective_at: None,
            expected: edge_catalog::SnapshotCounts {
                tax_profiles: i64::try_from(self.profiles.len()).unwrap_or(i64::MAX),
                products: i64::try_from(products.len()).unwrap_or(i64::MAX),
                barcodes: i64::try_from(barcodes.len()).unwrap_or(i64::MAX),
            },
            tax_profiles: self.profiles.clone(),
        })?;
        let snapshot_id = opened.snapshot.id();
        // Identical content that is already committed needs no chunks.
        if opened.snapshot.status() == edge_catalog::CatalogSnapshotStatus::Importing {
            app.put_catalog_chunk(PutCatalogChunkCommand {
                snapshot_id,
                chunk_index: 0,
                chunk_hash: hash_chunk(&products, &barcodes),
                products,
                barcodes,
            })?;
        }
        app.commit_catalog_import(CommitCatalogImportCommand {
            request_id: None,
            snapshot_id,
            chunk_count: 1,
        })
    }

    /// Import and put in force, which is what a sale needs.
    pub fn activate<S, C, I>(
        &self,
        app: &EdgeApplication<S, C, I>,
    ) -> Result<CatalogSnapshotId, AppError>
    where
        S: EdgeStore,
        C: Clock,
        I: IdFactoryPort,
    {
        let view = self.import(app)?;
        let snapshot_id = view.snapshot.id();
        app.activate_catalog_snapshot(ActivateCatalogSnapshotCommand {
            request_id: None,
            snapshot_id,
        })?;
        Ok(snapshot_id)
    }
}

/// Barcode of the default demo product.
pub const DEMO_BARCODE: &str = "7501000000001";
