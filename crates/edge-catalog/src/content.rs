//! Canonical content hashing.
//!
//! The `content_hash` of a snapshot is computed by the **core**, over the rows
//! it actually persisted, in a canonical order. The client's declared hash is
//! only ever compared against it — never trusted on its own, and never used as
//! the stored value.
//!
//! Encoding is length-prefixed so no field value can forge a boundary: every
//! field contributes its byte length followed by its bytes. Sections are
//! separated by a tag, and records are fed in a fixed order:
//!
//! 1. tax profiles, ascending by code,
//! 2. price books, ascending by `price_book_id`,
//! 3. products, ascending by `product_ref`,
//! 4. price entries, ascending by `(price_book_id, product_ref, valid_from)`,
//! 5. barcodes, ascending by barcode.
//!
//! Pricing is part of the hash: two snapshots that differ only in a price are
//! different snapshots.

use sha2::{Digest, Sha256};

use crate::pricing::{PriceBook, PriceEntry};
use crate::product::{CatalogBarcode, CatalogProduct};
use crate::tax::TaxProfile;

/// Prefix of the rendered hash, so the algorithm is visible in logs and
/// payloads.
pub const HASH_PREFIX: &str = "sha256";

/// Streaming hasher. The caller feeds rows in canonical order; nothing is
/// buffered, so a catalog of any size hashes in constant memory.
#[derive(Debug, Default)]
pub struct ContentHasher {
    hasher: Sha256,
}

impl ContentHasher {
    pub fn new() -> Self {
        Self::default()
    }

    fn field(&mut self, value: &[u8]) {
        self.hasher.update((value.len() as u64).to_be_bytes());
        self.hasher.update(value);
    }

    fn optional(&mut self, value: Option<&str>) {
        // A present empty string and an absent value must not collide.
        match value {
            Some(text) => {
                self.field(b"s");
                self.field(text.as_bytes());
            }
            None => self.field(b"n"),
        }
    }

    fn section(&mut self, tag: &str) {
        self.hasher.update(b"\x00section\x00");
        self.field(tag.as_bytes());
    }

    pub fn begin_tax_profiles(&mut self) {
        self.section("tax_profiles");
    }

    pub fn begin_price_books(&mut self) {
        self.section("price_books");
    }

    pub fn begin_products(&mut self) {
        self.section("products");
    }

    pub fn begin_price_entries(&mut self) {
        self.section("price_entries");
    }

    pub fn begin_barcodes(&mut self) {
        self.section("barcodes");
    }

    pub fn tax_profile(&mut self, profile: &TaxProfile) {
        self.field(profile.code.as_str().as_bytes());
        self.field(profile.name.as_bytes());
        self.field(profile.rate.bps().to_string().as_bytes());
    }

    pub fn price_book(&mut self, book: &PriceBook) {
        self.field(book.price_book_id.as_str().as_bytes());
        self.field(book.code.as_bytes());
        self.field(book.name.as_bytes());
        self.field(book.currency.code().as_bytes());
        self.field(book.scope.as_str().as_bytes());
        self.optional(book.scope.store_id().map(|id| id.as_str()));
        self.field(book.priority.to_string().as_bytes());
        self.field(if book.active { b"1" } else { b"0" });
    }

    pub fn product(&mut self, product: &CatalogProduct) {
        self.field(product.product_ref.as_str().as_bytes());
        self.optional(product.external_ref.as_deref());
        self.optional(product.sku.as_deref());
        self.field(product.name.as_bytes());
        self.field(if product.active { b"1" } else { b"0" });
        self.field(if product.sellable { b"1" } else { b"0" });
        self.field(product.tax_profile.as_str().as_bytes());
    }

    pub fn price_entry(&mut self, entry: &PriceEntry) {
        self.field(entry.price_book_id.as_str().as_bytes());
        self.field(entry.product_ref.as_str().as_bytes());
        self.field(entry.amount.minor().to_string().as_bytes());
        self.field(entry.amount.currency().code().as_bytes());
        self.field(entry.price_mode.as_str().as_bytes());
        self.optional(entry.valid_from.map(|ts| ts.to_rfc3339()).as_deref());
        self.optional(entry.valid_until.map(|ts| ts.to_rfc3339()).as_deref());
    }

    pub fn barcode(&mut self, barcode: &CatalogBarcode) {
        self.field(barcode.barcode.as_str().as_bytes());
        self.field(barcode.product_ref.as_str().as_bytes());
    }

    /// Render as `sha256:<hex>`.
    pub fn finish(self) -> String {
        let digest = self.hasher.finalize();
        let mut hex = String::with_capacity(digest.len() * 2 + HASH_PREFIX.len() + 1);
        hex.push_str(HASH_PREFIX);
        hex.push(':');
        for byte in digest {
            hex.push_str(&format!("{byte:02x}"));
        }
        hex
    }
}

/// Canonical ordering of price entries.
fn entry_key(entry: &PriceEntry) -> (String, String, String) {
    (
        entry.price_book_id.to_string(),
        entry.product_ref.to_string(),
        entry
            .valid_from
            .map(|ts| ts.to_rfc3339())
            .unwrap_or_default(),
    )
}

/// All the content of one snapshot.
#[derive(Debug, Clone, Default)]
pub struct SnapshotContent<'a> {
    pub tax_profiles: &'a [TaxProfile],
    pub price_books: &'a [PriceBook],
    pub products: &'a [CatalogProduct],
    pub price_entries: &'a [PriceEntry],
    pub barcodes: &'a [CatalogBarcode],
}

/// Hash a whole snapshot. Used by a producer (the server, the CLI importer) to
/// declare what it is sending.
pub fn hash_snapshot(content: SnapshotContent<'_>) -> String {
    let mut profiles: Vec<&TaxProfile> = content.tax_profiles.iter().collect();
    profiles.sort_by(|a, b| a.code.cmp(&b.code));
    let mut books: Vec<&PriceBook> = content.price_books.iter().collect();
    books.sort_by(|a, b| a.price_book_id.cmp(&b.price_book_id));
    let mut products: Vec<&CatalogProduct> = content.products.iter().collect();
    products.sort_by(|a, b| a.product_ref.cmp(&b.product_ref));
    let mut entries: Vec<&PriceEntry> = content.price_entries.iter().collect();
    entries.sort_by_key(|entry| entry_key(entry));
    let mut barcodes: Vec<&CatalogBarcode> = content.barcodes.iter().collect();
    barcodes.sort_by(|a, b| a.barcode.cmp(&b.barcode));

    let mut hasher = ContentHasher::new();
    hasher.begin_tax_profiles();
    for profile in profiles {
        hasher.tax_profile(profile);
    }
    hasher.begin_price_books();
    for book in books {
        hasher.price_book(book);
    }
    hasher.begin_products();
    for product in products {
        hasher.product(product);
    }
    hasher.begin_price_entries();
    for entry in entries {
        hasher.price_entry(entry);
    }
    hasher.begin_barcodes();
    for barcode in barcodes {
        hasher.barcode(barcode);
    }
    hasher.finish()
}

/// Hash of one import chunk's canonical content.
///
/// Used only to decide whether a replayed chunk carries the same bytes as the
/// one already stored. Snapshot integrity is the core's own computation over
/// persisted rows, never this.
pub fn hash_chunk(
    products: &[CatalogProduct],
    price_entries: &[PriceEntry],
    barcodes: &[CatalogBarcode],
) -> String {
    hash_snapshot(SnapshotContent {
        tax_profiles: &[],
        price_books: &[],
        products,
        price_entries,
        barcodes,
    })
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::{NewPriceBook, NewPriceEntry, PriceBookScope};
    use crate::product::{Barcode, NewCatalogProduct};
    use crate::tax::TaxProfileCode;
    use edge_domain::prelude::{
        Currency, Money, PriceBookId, PriceMode, ProductRef, StoreId, TaxRate, Timestamp,
    };

    fn profile(code: &str, bps: u32) -> TaxProfile {
        TaxProfile::new(
            TaxProfileCode::new(code).unwrap(),
            format!("profile {code}"),
            TaxRate::from_bps(bps).unwrap(),
        )
        .unwrap()
    }

    fn book(id: &str, priority: i32) -> PriceBook {
        PriceBook::import(NewPriceBook {
            price_book_id: PriceBookId::new(id).unwrap(),
            code: id.to_owned(),
            name: format!("book {id}"),
            currency: Currency::Mxn,
            scope: PriceBookScope::Tenant,
            priority,
            active: true,
        })
        .unwrap()
    }

    fn product(reference: &str) -> CatalogProduct {
        CatalogProduct::import(NewCatalogProduct {
            product_ref: ProductRef::new(reference).unwrap(),
            external_ref: None,
            sku: Some(format!("SKU-{reference}")),
            name: format!("Product {reference}"),
            active: true,
            sellable: true,
            tax_profile: TaxProfileCode::new("IVA16").unwrap(),
        })
        .unwrap()
    }

    fn entry(book: &PriceBook, reference: &str, minor: i64) -> PriceEntry {
        PriceEntry::import(NewPriceEntry {
            price_book_id: book.price_book_id.clone(),
            product_ref: ProductRef::new(reference).unwrap(),
            amount: Money::from_minor(minor, Currency::Mxn),
            price_mode: PriceMode::TaxExclusive,
            valid_from: None,
            valid_until: None,
        })
        .unwrap()
    }

    fn barcode(code: &str, reference: &str) -> CatalogBarcode {
        CatalogBarcode {
            barcode: Barcode::new(code).unwrap(),
            product_ref: ProductRef::new(reference).unwrap(),
        }
    }

    fn content<'a>(
        profiles: &'a [TaxProfile],
        books: &'a [PriceBook],
        products: &'a [CatalogProduct],
        entries: &'a [PriceEntry],
        barcodes: &'a [CatalogBarcode],
    ) -> SnapshotContent<'a> {
        SnapshotContent {
            tax_profiles: profiles,
            price_books: books,
            products,
            price_entries: entries,
            barcodes,
        }
    }

    #[test]
    fn hash_is_stable_and_order_independent() {
        let profiles = vec![profile("IVA16", 1_600), profile("IVA0", 0)];
        let books = vec![book("B", 1), book("A", 0)];
        let products = vec![product("P-2"), product("P-1")];
        let entries = vec![entry(&books[0], "P-2", 200), entry(&books[1], "P-1", 100)];
        let barcodes = vec![barcode("22222", "P-2"), barcode("11111", "P-1")];

        let first = hash_snapshot(content(&profiles, &books, &products, &entries, &barcodes));

        let r_profiles: Vec<TaxProfile> = profiles.iter().rev().cloned().collect();
        let r_books: Vec<PriceBook> = books.iter().rev().cloned().collect();
        let r_products: Vec<CatalogProduct> = products.iter().rev().cloned().collect();
        let r_entries: Vec<PriceEntry> = entries.iter().rev().cloned().collect();
        let r_barcodes: Vec<CatalogBarcode> = barcodes.iter().rev().cloned().collect();
        let second = hash_snapshot(content(
            &r_profiles,
            &r_books,
            &r_products,
            &r_entries,
            &r_barcodes,
        ));

        assert_eq!(first, second);
        assert!(first.starts_with("sha256:"));
        assert_eq!(first.len(), "sha256:".len() + 64);
    }

    #[test]
    fn pricing_is_part_of_the_hash() {
        let profiles = vec![profile("IVA16", 1_600)];
        let books = vec![book("BASE", 0)];
        let products = vec![product("P-1")];
        let barcodes = vec![barcode("11111", "P-1")];
        let entries = vec![entry(&books[0], "P-1", 100)];
        let baseline = hash_snapshot(content(&profiles, &books, &products, &entries, &barcodes));

        // A different amount is a different snapshot.
        let dearer = vec![entry(&books[0], "P-1", 101)];
        assert_ne!(
            baseline,
            hash_snapshot(content(&profiles, &books, &products, &dearer, &barcodes))
        );

        // So is a different price mode.
        let mut inclusive = entries.clone();
        inclusive[0].price_mode = PriceMode::TaxInclusive;
        assert_ne!(
            baseline,
            hash_snapshot(content(&profiles, &books, &products, &inclusive, &barcodes))
        );

        // So is a schedule.
        let mut scheduled = entries.clone();
        scheduled[0].valid_from = Some(Timestamp::parse_rfc3339("2026-11-01T00:00:00Z").unwrap());
        assert_ne!(
            baseline,
            hash_snapshot(content(&profiles, &books, &products, &scheduled, &barcodes))
        );

        // And so is the book that carries it.
        let other_scope = vec![PriceBook {
            scope: PriceBookScope::Store {
                store_id: StoreId::new("S-1").unwrap(),
            },
            ..books[0].clone()
        }];
        assert_ne!(
            baseline,
            hash_snapshot(content(
                &profiles,
                &other_scope,
                &products,
                &entries,
                &barcodes
            ))
        );
    }

    #[test]
    fn absent_and_empty_optional_fields_do_not_collide() {
        let profiles = vec![profile("IVA16", 1_600)];
        let books = vec![book("BASE", 0)];
        let entries = vec![entry(&books[0], "P-1", 100)];
        let mut without_sku = product("P-1");
        without_sku.sku = None;
        let mut empty_sku = product("P-1");
        empty_sku.sku = Some(String::new());
        assert_ne!(
            hash_snapshot(content(&profiles, &books, &[without_sku], &entries, &[])),
            hash_snapshot(content(&profiles, &books, &[empty_sku], &entries, &[]))
        );
    }

    #[test]
    fn field_boundaries_cannot_be_forged() {
        let books = vec![book("BASE", 0)];
        let entries = vec![entry(&books[0], "P-1", 100)];
        let mut first = product("P-1");
        first.name = "AB".to_owned();
        first.sku = Some("C".to_owned());
        let mut second = product("P-1");
        second.name = "A".to_owned();
        second.sku = Some("BC".to_owned());
        assert_ne!(
            hash_snapshot(content(&[], &books, &[first], &entries, &[])),
            hash_snapshot(content(&[], &books, &[second], &entries, &[]))
        );
    }

    #[test]
    fn streaming_matches_the_in_memory_helper() {
        let profiles = vec![profile("IVA16", 1_600)];
        let books = vec![book("BASE", 0)];
        let products = vec![product("P-1"), product("P-2")];
        let entries = vec![entry(&books[0], "P-1", 100), entry(&books[0], "P-2", 200)];
        let barcodes = vec![barcode("11111", "P-1")];

        let mut hasher = ContentHasher::new();
        hasher.begin_tax_profiles();
        hasher.tax_profile(&profiles[0]);
        hasher.begin_price_books();
        hasher.price_book(&books[0]);
        hasher.begin_products();
        hasher.product(&products[0]);
        hasher.product(&products[1]);
        hasher.begin_price_entries();
        hasher.price_entry(&entries[0]);
        hasher.price_entry(&entries[1]);
        hasher.begin_barcodes();
        hasher.barcode(&barcodes[0]);

        assert_eq!(
            hasher.finish(),
            hash_snapshot(content(&profiles, &books, &products, &entries, &barcodes))
        );
    }
}
