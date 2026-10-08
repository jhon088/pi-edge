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

use edge_catalog::pricing::{NewPriceBook, NewPriceEntry, PriceBookScope};
use edge_catalog::product::NewCatalogProduct;
use edge_catalog::{
    hash_chunk, hash_snapshot, Barcode, CatalogBarcode, CatalogProduct, Currency, Money, PriceBook,
    PriceEntry, PriceMode, ProductRef, SnapshotContent, StoreId, TaxProfile, TaxProfileCode,
    TaxRate,
};
use edge_domain::prelude::{CatalogSnapshotId, PriceBookId};

use crate::commands::{
    ActivateCatalogSnapshotCommand, BeginCatalogImportCommand, CommitCatalogImportCommand,
    PutCatalogChunkCommand,
};
use crate::error::AppError;
use crate::ports::{EdgeStore, IdFactory as IdFactoryPort};
use crate::queries::CatalogSnapshotView;
use crate::service::EdgeApplication;

/// Barcode of the default demo product.
pub const DEMO_BARCODE: &str = "7501000000001";
/// Id of the tenant-wide base price book every fixture starts with.
pub const BASE_BOOK: &str = "BASE";

/// One price for a fixture product.
#[derive(Debug, Clone)]
pub struct FixturePrice {
    pub book: &'static str,
    pub amount_minor: i64,
    pub price_mode: PriceMode,
    pub valid_from: Option<Timestamp>,
    pub valid_until: Option<Timestamp>,
}

impl FixturePrice {
    pub fn base(amount_minor: i64) -> Self {
        Self {
            book: BASE_BOOK,
            amount_minor,
            price_mode: PriceMode::TaxExclusive,
            valid_from: None,
            valid_until: None,
        }
    }

    pub fn in_book(mut self, book: &'static str) -> Self {
        self.book = book;
        self
    }

    pub fn inclusive(mut self) -> Self {
        self.price_mode = PriceMode::TaxInclusive;
        self
    }

    pub fn from(mut self, valid_from: Timestamp) -> Self {
        self.valid_from = Some(valid_from);
        self
    }

    pub fn until(mut self, valid_until: Timestamp) -> Self {
        self.valid_until = Some(valid_until);
        self
    }
}

/// One product in a test snapshot, with the prices that travel with it.
#[derive(Debug, Clone)]
pub struct FixtureProduct {
    pub product_ref: &'static str,
    pub name: &'static str,
    pub sku: Option<&'static str>,
    pub barcodes: Vec<&'static str>,
    pub tax_profile: &'static str,
    pub active: bool,
    pub sellable: bool,
    pub prices: Vec<FixturePrice>,
}

impl FixtureProduct {
    /// A sellable product at 16% tax, priced in the base book, tax exclusive.
    pub fn new(product_ref: &'static str, name: &'static str, price_minor: i64) -> Self {
        Self {
            product_ref,
            name,
            sku: None,
            barcodes: Vec::new(),
            tax_profile: "IVA16",
            active: true,
            sellable: true,
            prices: vec![FixturePrice::base(price_minor)],
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

    /// Replace the default price.
    pub fn priced(mut self, price: FixturePrice) -> Self {
        self.prices = vec![price];
        self
    }

    /// Add another price, in another book or another validity window.
    pub fn plus_price(mut self, price: FixturePrice) -> Self {
        self.prices.push(price);
        self
    }

    /// A product the catalog knows but nobody priced.
    pub fn unpriced(mut self) -> Self {
        self.prices.clear();
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
    pub books: Vec<PriceBook>,
    pub products: Vec<FixtureProduct>,
}

#[allow(clippy::expect_used)]
fn profile(code: &str, bps: u32) -> TaxProfile {
    TaxProfile::new(
        TaxProfileCode::new(code).expect("valid code"),
        format!("profile {code}"),
        TaxRate::from_bps(bps).expect("valid rate"),
    )
    .expect("valid profile")
}

#[allow(clippy::expect_used)]
fn book(id: &str, scope: PriceBookScope, priority: i32) -> PriceBook {
    PriceBook::import(NewPriceBook {
        price_book_id: PriceBookId::new(id).expect("valid id"),
        code: id.to_owned(),
        name: format!("book {id}"),
        currency: Currency::Mxn,
        scope,
        priority,
        active: true,
    })
    .expect("valid book")
}

impl CatalogFixture {
    /// A snapshot pre-loaded with the Mexican rates we care about and a
    /// tenant-wide base price book.
    pub fn new(version: i64) -> Self {
        Self {
            version,
            currency: Currency::Mxn,
            #[allow(clippy::expect_used)]
            generated_at: Timestamp::parse_rfc3339("2026-10-01T00:00:00Z")
                .expect("valid timestamp"),
            profiles: vec![
                profile("IVA16", 1_600),
                profile("IVA8", 800),
                profile("IVA0", 0),
            ],
            books: vec![book(BASE_BOOK, PriceBookScope::Tenant, 0)],
            products: Vec::new(),
        }
    }

    /// The default single-product catalog used by the sale tests: 100.00 MXN
    /// at 16%, tax exclusive, from the base book.
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

    /// Add a price book scoped to one store.
    pub fn with_store_book(mut self, id: &'static str, store_id: &str, priority: i32) -> Self {
        #[allow(clippy::expect_used)]
        let store_id = StoreId::new(store_id).expect("valid store id");
        self.books
            .push(book(id, PriceBookScope::Store { store_id }, priority));
        self
    }

    /// Add a tenant-wide price book with an explicit priority.
    pub fn with_tenant_book(mut self, id: &'static str, priority: i32) -> Self {
        self.books.push(book(id, PriceBookScope::Tenant, priority));
        self
    }

    /// Replace a book with an inactive copy of itself.
    pub fn deactivate_book(mut self, id: &str) -> Self {
        for existing in &mut self.books {
            if existing.price_book_id.as_str() == id {
                existing.active = false;
            }
        }
        self
    }

    #[allow(clippy::expect_used)]
    fn records(&self) -> (Vec<CatalogProduct>, Vec<PriceEntry>, Vec<CatalogBarcode>) {
        let mut products = Vec::new();
        let mut prices = Vec::new();
        let mut barcodes = Vec::new();
        for fixture in &self.products {
            let product_ref = ProductRef::new(fixture.product_ref).expect("valid product ref");
            products.push(
                CatalogProduct::import(NewCatalogProduct {
                    product_ref: product_ref.clone(),
                    external_ref: None,
                    sku: fixture.sku.map(str::to_owned),
                    name: fixture.name.to_owned(),
                    active: fixture.active,
                    sellable: fixture.sellable,
                    tax_profile: TaxProfileCode::new(fixture.tax_profile).expect("valid code"),
                })
                .expect("valid product"),
            );
            for price in &fixture.prices {
                prices.push(
                    PriceEntry::import(NewPriceEntry {
                        price_book_id: PriceBookId::new(price.book).expect("valid book id"),
                        product_ref: product_ref.clone(),
                        amount: Money::from_minor(price.amount_minor, self.currency),
                        price_mode: price.price_mode,
                        valid_from: price.valid_from,
                        valid_until: price.valid_until,
                    })
                    .expect("valid price"),
                );
            }
            for code in &fixture.barcodes {
                barcodes.push(CatalogBarcode {
                    barcode: Barcode::new(*code).expect("valid barcode"),
                    product_ref: product_ref.clone(),
                });
            }
        }
        (products, prices, barcodes)
    }

    fn counts(
        &self,
        products: usize,
        prices: usize,
        barcodes: usize,
    ) -> edge_catalog::SnapshotCounts {
        edge_catalog::SnapshotCounts {
            tax_profiles: i64::try_from(self.profiles.len()).unwrap_or(i64::MAX),
            price_books: i64::try_from(self.books.len()).unwrap_or(i64::MAX),
            products: i64::try_from(products).unwrap_or(i64::MAX),
            price_entries: i64::try_from(prices).unwrap_or(i64::MAX),
            barcodes: i64::try_from(barcodes).unwrap_or(i64::MAX),
        }
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
        let (products, price_entries, barcodes) = self.records();
        let opened = app.begin_catalog_import(BeginCatalogImportCommand {
            request_id: None,
            version: self.version,
            currency: self.currency,
            declared_content_hash: hash_snapshot(SnapshotContent {
                tax_profiles: &self.profiles,
                price_books: &self.books,
                products: &products,
                price_entries: &price_entries,
                barcodes: &barcodes,
            }),
            generated_at: self.generated_at,
            effective_at: None,
            expected: self.counts(products.len(), price_entries.len(), barcodes.len()),
            tax_profiles: self.profiles.clone(),
            price_books: self.books.clone(),
        })?;
        let snapshot_id = opened.snapshot.id();
        // Identical content that is already committed needs no chunks.
        if opened.snapshot.status() == edge_catalog::CatalogSnapshotStatus::Importing {
            app.put_catalog_chunk(PutCatalogChunkCommand {
                snapshot_id,
                chunk_index: 0,
                chunk_hash: hash_chunk(&products, &price_entries, &barcodes),
                products,
                price_entries,
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
