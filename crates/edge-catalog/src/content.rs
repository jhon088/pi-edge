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
//! 2. products, ascending by `product_ref`,
//! 3. barcodes, ascending by barcode.

use sha2::{Digest, Sha256};

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

    fn section(&mut self, tag: &str) {
        self.hasher.update(b"\x00section\x00");
        self.field(tag.as_bytes());
    }

    pub fn begin_tax_profiles(&mut self) {
        self.section("tax_profiles");
    }

    pub fn begin_products(&mut self) {
        self.section("products");
    }

    pub fn begin_barcodes(&mut self) {
        self.section("barcodes");
    }

    pub fn tax_profile(&mut self, profile: &TaxProfile) {
        self.field(profile.code.as_str().as_bytes());
        self.field(profile.name.as_bytes());
        self.field(profile.rate.bps().to_string().as_bytes());
        self.field(profile.price_mode.as_str().as_bytes());
    }

    pub fn product(&mut self, product: &CatalogProduct) {
        self.field(product.product_ref.as_str().as_bytes());
        self.field(product.external_ref.as_deref().unwrap_or("").as_bytes());
        self.field(product.sku.as_deref().unwrap_or("").as_bytes());
        self.field(product.name.as_bytes());
        self.field(if product.active { b"1" } else { b"0" });
        self.field(if product.sellable { b"1" } else { b"0" });
        self.field(product.tax_profile.as_str().as_bytes());
        self.field(product.unit_price.minor().to_string().as_bytes());
        self.field(product.unit_price.currency().code().as_bytes());
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

/// Convenience for callers that already hold the whole snapshot in memory
/// (fixtures, tests, the CLI importer computing its declared hash).
pub fn hash_snapshot(
    tax_profiles: &[TaxProfile],
    products: &[CatalogProduct],
    barcodes: &[CatalogBarcode],
) -> String {
    let mut profiles: Vec<&TaxProfile> = tax_profiles.iter().collect();
    profiles.sort_by(|a, b| a.code.cmp(&b.code));
    let mut sorted_products: Vec<&CatalogProduct> = products.iter().collect();
    sorted_products.sort_by(|a, b| a.product_ref.cmp(&b.product_ref));
    let mut sorted_barcodes: Vec<&CatalogBarcode> = barcodes.iter().collect();
    sorted_barcodes.sort_by(|a, b| a.barcode.cmp(&b.barcode));

    let mut hasher = ContentHasher::new();
    hasher.begin_tax_profiles();
    for profile in profiles {
        hasher.tax_profile(profile);
    }
    hasher.begin_products();
    for product in sorted_products {
        hasher.product(product);
    }
    hasher.begin_barcodes();
    for barcode in sorted_barcodes {
        hasher.barcode(barcode);
    }
    hasher.finish()
}

/// Hash of one import chunk's canonical content.
///
/// Used only to decide whether a replayed chunk carries the same bytes as the
/// one already stored. Snapshot integrity is the core's own
/// [`hash_snapshot`]-shaped computation over persisted rows, never this.
pub fn hash_chunk(products: &[CatalogProduct], barcodes: &[CatalogBarcode]) -> String {
    let mut sorted_products: Vec<&CatalogProduct> = products.iter().collect();
    sorted_products.sort_by(|a, b| a.product_ref.cmp(&b.product_ref));
    let mut sorted_barcodes: Vec<&CatalogBarcode> = barcodes.iter().collect();
    sorted_barcodes.sort_by(|a, b| a.barcode.cmp(&b.barcode));

    let mut hasher = ContentHasher::new();
    hasher.begin_products();
    for product in sorted_products {
        hasher.product(product);
    }
    hasher.begin_barcodes();
    for barcode in sorted_barcodes {
        hasher.barcode(barcode);
    }
    hasher.finish()
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::{Barcode, NewCatalogProduct};
    use crate::tax::TaxProfileCode;
    use edge_domain::prelude::{Currency, Money, PriceMode, ProductRef, TaxRate};

    fn profile(code: &str, bps: u32) -> TaxProfile {
        TaxProfile::new(
            TaxProfileCode::new(code).unwrap(),
            format!("profile {code}"),
            TaxRate::from_bps(bps).unwrap(),
            PriceMode::TaxExclusive,
        )
        .unwrap()
    }

    fn product(reference: &str, price: i64) -> CatalogProduct {
        CatalogProduct::import(NewCatalogProduct {
            product_ref: ProductRef::new(reference).unwrap(),
            external_ref: None,
            sku: Some(format!("SKU-{reference}")),
            name: format!("Product {reference}"),
            active: true,
            sellable: true,
            tax_profile: TaxProfileCode::new("IVA16").unwrap(),
            unit_price: Money::from_minor(price, Currency::Mxn),
        })
        .unwrap()
    }

    fn barcode(code: &str, reference: &str) -> CatalogBarcode {
        CatalogBarcode {
            barcode: Barcode::new(code).unwrap(),
            product_ref: ProductRef::new(reference).unwrap(),
        }
    }

    #[test]
    fn hash_is_stable_and_order_independent() {
        let profiles = vec![profile("IVA16", 1_600), profile("IVA0", 0)];
        let products = vec![product("P-2", 200), product("P-1", 100)];
        let barcodes = vec![barcode("22222", "P-2"), barcode("11111", "P-1")];

        let first = hash_snapshot(&profiles, &products, &barcodes);

        let reversed_profiles: Vec<TaxProfile> = profiles.iter().rev().cloned().collect();
        let reversed_products: Vec<CatalogProduct> = products.iter().rev().cloned().collect();
        let reversed_barcodes: Vec<CatalogBarcode> = barcodes.iter().rev().cloned().collect();
        let second = hash_snapshot(&reversed_profiles, &reversed_products, &reversed_barcodes);

        assert_eq!(first, second);
        assert!(first.starts_with("sha256:"));
        assert_eq!(first.len(), "sha256:".len() + 64);
    }

    #[test]
    fn hash_changes_when_any_field_changes() {
        let profiles = vec![profile("IVA16", 1_600)];
        let products = vec![product("P-1", 100)];
        let barcodes = vec![barcode("11111", "P-1")];
        let baseline = hash_snapshot(&profiles, &products, &barcodes);

        assert_ne!(
            baseline,
            hash_snapshot(&profiles, &[product("P-1", 101)], &barcodes),
            "a price change must change the hash"
        );
        assert_ne!(
            baseline,
            hash_snapshot(&[profile("IVA16", 800)], &products, &barcodes),
            "a rate change must change the hash"
        );
        assert_ne!(
            baseline,
            hash_snapshot(&profiles, &products, &[barcode("11112", "P-1")]),
            "a barcode change must change the hash"
        );
    }

    #[test]
    fn field_boundaries_cannot_be_forged() {
        // Two products whose concatenated fields would collide without length
        // prefixes: ("AB","C") versus ("A","BC").
        let mut first = product("P-1", 100);
        first.name = "AB".to_owned();
        first.sku = Some("C".to_owned());
        let mut second = product("P-1", 100);
        second.name = "A".to_owned();
        second.sku = Some("BC".to_owned());
        assert_ne!(
            hash_snapshot(&[], &[first], &[]),
            hash_snapshot(&[], &[second], &[])
        );
    }

    #[test]
    fn streaming_matches_the_in_memory_helper() {
        let profiles = vec![profile("IVA16", 1_600)];
        let products = vec![product("P-1", 100), product("P-2", 200)];
        let barcodes = vec![barcode("11111", "P-1")];

        let mut hasher = ContentHasher::new();
        hasher.begin_tax_profiles();
        hasher.tax_profile(&profiles[0]);
        hasher.begin_products();
        hasher.product(&products[0]);
        hasher.product(&products[1]);
        hasher.begin_barcodes();
        hasher.barcode(&barcodes[0]);

        assert_eq!(
            hasher.finish(),
            hash_snapshot(&profiles, &products, &barcodes)
        );
    }
}
