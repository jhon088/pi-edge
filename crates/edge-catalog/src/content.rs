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
//! 5. barcodes, ascending by barcode,
//! 6. promotions, ascending by `promotion_id`,
//! 7. channels, ascending by `channel_ref`,
//! 8. categories, ascending by `category_ref`,
//! 9. product presentation, ascending by `product_ref`,
//! 10. selection groups, ascending by `group_id`,
//! 11. selection options, ascending by `(group_id, option_id)`,
//! 12. channel availability, ascending by `(channel_ref, target, reference)`,
//! 13. product recommendations, ascending by `product_ref`, each row's refs in
//!     the order the catalog declared them.
//!
//! Pricing is part of the hash: two snapshots that differ only in a price are
//! different snapshots. So is promotional content: a snapshot that discounts a
//! product is not the snapshot that sells it at list price. So is presentation:
//! a different photo or a different translation is a different snapshot, which
//! is what lets a client cache by hash (ADR 044).
//!
//! Sections 7 to 13 contribute **nothing at all** when they carry no rows.
//! That is deliberate: a Retail catalog that uses none of the universal
//! extensions keeps the hash it had before they existed, so a snapshot already
//! activated in a store still matches what an upgraded producer declares for
//! the same content. Two semantically equal snapshots hash identically either
//! way, because each section announces itself with its own tag.

use sha2::{Digest, Sha256};

use crate::channel::{Channel, ChannelAvailability};
use crate::presentation::{AssetRef, Category, LocalizedText, ProductPresentation};
use crate::pricing::{PriceBook, PriceEntry};
use crate::product::{CatalogBarcode, CatalogProduct};
use crate::promotions::{Promotion, PromotionRule};
use crate::selection::{SelectionGroup, SelectionOption, SelectionOwner};
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

    pub fn begin_promotions(&mut self) {
        self.section("promotions");
    }

    pub fn begin_channels(&mut self) {
        self.section("channels");
    }

    pub fn begin_categories(&mut self) {
        self.section("categories");
    }

    pub fn begin_presentation(&mut self) {
        self.section("presentation");
    }

    pub fn begin_selection_groups(&mut self) {
        self.section("selection_groups");
    }

    pub fn begin_recommendations(&mut self) {
        self.section("recommendations");
    }

    /// One product's recommendation list, in declared order. Order is content:
    /// what a screen offers first is a merchandising decision.
    pub fn recommendations(&mut self, presentation: &ProductPresentation) {
        self.field(presentation.product_ref.as_str().as_bytes());
        self.field(
            presentation
                .recommended_product_refs
                .len()
                .to_string()
                .as_bytes(),
        );
        for recommended in &presentation.recommended_product_refs {
            self.field(recommended.as_str().as_bytes());
        }
    }

    pub fn begin_selection_options(&mut self) {
        self.section("selection_options");
    }

    pub fn begin_availability(&mut self) {
        self.section("availability");
    }

    /// Localized text contributes its default and then every translation in
    /// locale order, so the inbound order of a JSON object cannot change the
    /// hash. The count is fed first: `{en: "a b"}` and `{en: "a", x: "b"}`
    /// cannot encode to the same bytes anyway, but the count makes the record
    /// self-describing.
    fn localized(&mut self, text: &LocalizedText) {
        self.field(text.default.as_bytes());
        self.field(text.translations.len().to_string().as_bytes());
        for (locale, translation) in &text.translations {
            self.field(locale.as_str().as_bytes());
            self.field(translation.as_bytes());
        }
    }

    fn optional_localized(&mut self, text: Option<&LocalizedText>) {
        match text {
            Some(value) => {
                self.field(b"s");
                self.localized(value);
            }
            None => self.field(b"n"),
        }
    }

    /// An asset contributes its content hash, not its bytes: PI.Edge holds the
    /// reference, the Asset Runtime holds the file (ADR 044).
    fn optional_asset(&mut self, asset: Option<&AssetRef>) {
        match asset {
            Some(value) => {
                self.field(b"s");
                self.field(value.content_hash.as_bytes());
                self.field(value.media_type.as_bytes());
                self.field(value.byte_size.to_string().as_bytes());
            }
            None => self.field(b"n"),
        }
    }

    pub fn channel(&mut self, channel: &Channel) {
        self.field(channel.channel_ref.as_str().as_bytes());
        self.localized(&channel.name);
        self.field(channel.default_availability.as_str().as_bytes());
    }

    pub fn category(&mut self, category: &Category) {
        self.field(category.category_ref.as_str().as_bytes());
        self.localized(&category.name);
        self.field(category.display_order.to_string().as_bytes());
        self.optional_asset(category.image.as_ref());
    }

    pub fn presentation(&mut self, presentation: &ProductPresentation) {
        self.field(presentation.product_ref.as_str().as_bytes());
        self.optional(
            presentation
                .category_ref
                .as_ref()
                .map(|category| category.as_str()),
        );
        self.optional_localized(presentation.display_name.as_ref());
        self.optional_localized(presentation.description.as_ref());
        self.field(presentation.display_order.to_string().as_bytes());
        self.optional_asset(presentation.image.as_ref());
    }

    pub fn selection_group(&mut self, group: &SelectionGroup) {
        self.field(group.group_id.as_str().as_bytes());
        // Owner kind and owner reference are separate fields: a product and an
        // option that happen to share a string are still different owners.
        match &group.owner {
            SelectionOwner::Product { product_ref } => {
                self.field(b"PRODUCT");
                self.field(product_ref.as_str().as_bytes());
            }
            SelectionOwner::Option { option_id } => {
                self.field(b"OPTION");
                self.field(option_id.as_str().as_bytes());
            }
        }
        self.localized(&group.name);
        self.field(group.min_quantity.to_string().as_bytes());
        self.field(group.max_quantity.to_string().as_bytes());
        self.field(group.display_order.to_string().as_bytes());
    }

    pub fn selection_option(&mut self, option: &SelectionOption) {
        self.field(option.option_id.as_str().as_bytes());
        self.field(option.group_id.as_str().as_bytes());
        self.localized(&option.name);
        self.optional(
            option
                .component_ref
                .as_ref()
                .map(|product| product.as_str()),
        );
        self.field(option.max_quantity.to_string().as_bytes());
        self.field(option.default_quantity.to_string().as_bytes());
        self.field(option.display_order.to_string().as_bytes());
        self.optional_asset(option.image.as_ref());
    }

    pub fn availability(&mut self, row: &ChannelAvailability) {
        self.field(row.channel_ref.as_str().as_bytes());
        self.field(row.target.as_str().as_bytes());
        self.field(row.target.reference().as_bytes());
        self.field(row.state.as_str().as_bytes());
        self.optional(
            row.state
                .snoozed_until()
                .map(|until| until.to_rfc3339())
                .as_deref(),
        );
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

    pub fn promotion(&mut self, promotion: &Promotion) {
        self.field(promotion.promotion_id.as_str().as_bytes());
        self.field(promotion.code.as_bytes());
        self.field(promotion.name.as_bytes());
        self.field(if promotion.active { b"1" } else { b"0" });
        self.field(promotion.priority.to_string().as_bytes());
        self.field(promotion.stackability.as_str().as_bytes());
        self.field(promotion.scope.as_str().as_bytes());
        self.optional(promotion.scope.store_id().map(|id| id.as_str()));
        self.optional(promotion.valid_from.map(|ts| ts.to_rfc3339()).as_deref());
        self.optional(promotion.valid_until.map(|ts| ts.to_rfc3339()).as_deref());
        // Every rule contributes every slot, present or absent, so no two rule
        // shapes can encode to the same bytes.
        self.field(promotion.rule.kind().as_bytes());
        self.optional(
            promotion
                .rule
                .applicability()
                .and_then(|applies| applies.product_ref())
                .map(|product| product.as_str()),
        );
        let (percentage, amount, minimum, buy, get) = match &promotion.rule {
            PromotionRule::PercentageOff { percentage, .. } => {
                (Some(percentage.bps().to_string()), None, None, None, None)
            }
            PromotionRule::FixedAmountOff { amount, .. } => (
                None,
                Some(format!("{}:{}", amount.minor(), amount.currency().code())),
                None,
                None,
                None,
            ),
            PromotionRule::BuyNGetM { buy, get, .. } => (
                None,
                None,
                None,
                Some(buy.thousandths().to_string()),
                Some(get.thousandths().to_string()),
            ),
            PromotionRule::BasketThreshold { minimum, amount } => (
                None,
                Some(format!("{}:{}", amount.minor(), amount.currency().code())),
                Some(format!("{}:{}", minimum.minor(), minimum.currency().code())),
                None,
                None,
            ),
        };
        self.optional(percentage.as_deref());
        self.optional(amount.as_deref());
        self.optional(minimum.as_deref());
        self.optional(buy.as_deref());
        self.optional(get.as_deref());
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

/// Canonical ordering of selection options: by owning group, then by id.
/// Display order is content, not identity, so it never orders the hash.
fn option_key(option: &SelectionOption) -> (String, String) {
    (option.group_id.to_string(), option.option_id.to_string())
}

/// Canonical ordering of availability rows.
fn availability_key(row: &ChannelAvailability) -> (String, String, String) {
    (
        row.channel_ref.to_string(),
        row.target.as_str().to_owned(),
        row.target.reference().to_owned(),
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
    pub promotions: &'a [Promotion],
    pub channels: &'a [Channel],
    pub categories: &'a [Category],
    pub presentation: &'a [ProductPresentation],
    pub selection_groups: &'a [SelectionGroup],
    pub selection_options: &'a [SelectionOption],
    pub availability: &'a [ChannelAvailability],
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
    let mut promotions: Vec<&Promotion> = content.promotions.iter().collect();
    promotions.sort_by(|a, b| a.promotion_id.cmp(&b.promotion_id));

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
    hasher.begin_promotions();
    for promotion in promotions {
        hasher.promotion(promotion);
    }
    hash_extensions(&mut hasher, &content);
    hasher.finish()
}

/// Sections 7 to 13. Each is skipped entirely when it has no rows, so a
/// catalog that declares none of them hashes exactly as it did before they
/// existed.
fn hash_extensions(hasher: &mut ContentHasher, content: &SnapshotContent<'_>) {
    if !content.channels.is_empty() {
        let mut channels: Vec<&Channel> = content.channels.iter().collect();
        channels.sort_by(|a, b| a.channel_ref.cmp(&b.channel_ref));
        hasher.begin_channels();
        for channel in channels {
            hasher.channel(channel);
        }
    }
    if !content.categories.is_empty() {
        let mut categories: Vec<&Category> = content.categories.iter().collect();
        categories.sort_by(|a, b| a.category_ref.cmp(&b.category_ref));
        hasher.begin_categories();
        for category in categories {
            hasher.category(category);
        }
    }
    if !content.presentation.is_empty() {
        let mut presentation: Vec<&ProductPresentation> = content.presentation.iter().collect();
        presentation.sort_by(|a, b| a.product_ref.cmp(&b.product_ref));
        hasher.begin_presentation();
        for row in presentation {
            hasher.presentation(row);
        }
    }
    if !content.selection_groups.is_empty() {
        let mut groups: Vec<&SelectionGroup> = content.selection_groups.iter().collect();
        groups.sort_by(|a, b| a.group_id.cmp(&b.group_id));
        hasher.begin_selection_groups();
        for group in groups {
            hasher.selection_group(group);
        }
    }
    if !content.selection_options.is_empty() {
        let mut options: Vec<&SelectionOption> = content.selection_options.iter().collect();
        options.sort_by_key(|option| option_key(option));
        hasher.begin_selection_options();
        for option in options {
            hasher.selection_option(option);
        }
    }
    if !content.availability.is_empty() {
        let mut rows: Vec<&ChannelAvailability> = content.availability.iter().collect();
        rows.sort_by_key(|row| availability_key(row));
        hasher.begin_availability();
        for row in rows {
            hasher.availability(row);
        }
    }
    // Its own section rather than two more fields on `presentation`: a
    // snapshot that shows products but recommends nothing has to keep the
    // hash it had before recommendations existed.
    let mut recommending: Vec<&ProductPresentation> = content
        .presentation
        .iter()
        .filter(|row| !row.recommended_product_refs.is_empty())
        .collect();
    if !recommending.is_empty() {
        recommending.sort_by(|a, b| a.product_ref.cmp(&b.product_ref));
        hasher.begin_recommendations();
        for row in recommending {
            hasher.recommendations(row);
        }
    }
}

/// Hash of one import chunk's canonical content.
///
/// Used only to decide whether a replayed chunk carries the same bytes as the
/// one already stored. Snapshot integrity is the core's own computation over
/// persisted rows, never this.
pub fn hash_chunk(chunk: ChunkContent<'_>) -> String {
    hash_snapshot(SnapshotContent {
        products: chunk.products,
        price_entries: chunk.price_entries,
        barcodes: chunk.barcodes,
        promotions: chunk.promotions,
        presentation: chunk.presentation,
        selection_groups: chunk.selection_groups,
        selection_options: chunk.selection_options,
        availability: chunk.availability,
        ..SnapshotContent::default()
    })
}

/// What one import chunk may carry.
///
/// Deliberately narrower than [`SnapshotContent`]: tax profiles, price books,
/// channels and categories are declared once in the import header, so a chunk
/// cannot restate them and no caller can hash header content into a chunk.
#[derive(Debug, Clone, Default)]
pub struct ChunkContent<'a> {
    pub products: &'a [CatalogProduct],
    pub price_entries: &'a [PriceEntry],
    pub barcodes: &'a [CatalogBarcode],
    pub promotions: &'a [Promotion],
    pub presentation: &'a [ProductPresentation],
    pub selection_groups: &'a [SelectionGroup],
    pub selection_options: &'a [SelectionOption],
    pub availability: &'a [ChannelAvailability],
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::channel::{AvailabilityDefault, AvailabilityState, AvailabilityTarget};
    use crate::presentation::Locale;
    use crate::pricing::{NewPriceBook, NewPriceEntry, PriceBookScope};
    use crate::product::{Barcode, NewCatalogProduct};
    use crate::tax::TaxProfileCode;
    use edge_domain::prelude::{
        CategoryRef, ChannelRef, Currency, Money, PriceBookId, PriceMode, ProductRef,
        SelectionGroupId, SelectionOptionId, StoreId, TaxRate, Timestamp,
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
            ..SnapshotContent::default()
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
    fn promotional_content_is_part_of_the_hash() {
        use crate::promotions::{
            NewPromotion, ProductApplicability, Promotion, PromotionRule, Stackability,
        };
        use edge_domain::prelude::{Percentage, PromotionId, Quantity};

        let profiles = vec![profile("IVA16", 1_600)];
        let books = vec![book("BASE", 0)];
        let products = vec![product("P-1")];
        let entries = vec![entry(&books[0], "P-1", 100)];
        let base = SnapshotContent {
            tax_profiles: &profiles,
            price_books: &books,
            products: &products,
            price_entries: &entries,
            ..SnapshotContent::default()
        };

        let promotion = |rule: PromotionRule| {
            Promotion::import(NewPromotion {
                promotion_id: PromotionId::new("PR-1").unwrap(),
                code: "PR-1".to_owned(),
                name: "promo".to_owned(),
                active: true,
                priority: 0,
                stackability: Stackability::Stackable,
                scope: PriceBookScope::Tenant,
                valid_from: None,
                valid_until: None,
                rule,
            })
            .unwrap()
        };
        let ten_percent = vec![promotion(PromotionRule::PercentageOff {
            applies_to: ProductApplicability::AnyProduct,
            percentage: Percentage::from_bps(1_000).unwrap(),
        })];
        let twenty_percent = vec![promotion(PromotionRule::PercentageOff {
            applies_to: ProductApplicability::AnyProduct,
            percentage: Percentage::from_bps(2_000).unwrap(),
        })];
        let buy_two_get_one = vec![promotion(PromotionRule::BuyNGetM {
            applies_to: ProductApplicability::AnyProduct,
            buy: Quantity::from_units(2).unwrap(),
            get: Quantity::from_units(1).unwrap(),
        })];

        let without = hash_snapshot(base.clone());
        let with_ten = hash_snapshot(SnapshotContent {
            promotions: &ten_percent,
            ..base.clone()
        });
        let with_twenty = hash_snapshot(SnapshotContent {
            promotions: &twenty_percent,
            ..base.clone()
        });
        let with_bogo = hash_snapshot(SnapshotContent {
            promotions: &buy_two_get_one,
            ..base.clone()
        });

        // A promotion, its magnitude and its kind each change the snapshot.
        assert_ne!(without, with_ten);
        assert_ne!(with_ten, with_twenty);
        assert_ne!(with_ten, with_bogo);

        // Inactive is still different content from absent.
        let mut inactive = ten_percent.clone();
        inactive[0].active = false;
        assert_ne!(
            with_ten,
            hash_snapshot(SnapshotContent {
                promotions: &inactive,
                ..base.clone()
            })
        );
        assert_ne!(
            without,
            hash_snapshot(SnapshotContent {
                promotions: &inactive,
                ..base
            })
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
        hasher.begin_promotions();

        assert_eq!(
            hasher.finish(),
            hash_snapshot(content(&profiles, &books, &products, &entries, &barcodes))
        );
    }

    // ---- universal catalog extensions (ADR 044) ----

    fn locale(tag: &str) -> Locale {
        Locale::new(tag).unwrap()
    }

    fn text(default: &str) -> LocalizedText {
        LocalizedText::new(default).unwrap()
    }

    fn category(reference: &str, order: i32) -> Category {
        Category::import(
            CategoryRef::new(reference).unwrap(),
            text(reference),
            order,
            None,
        )
    }

    fn channel(reference: &str) -> Channel {
        Channel {
            channel_ref: ChannelRef::new(reference).unwrap(),
            name: text(reference),
            default_availability: AvailabilityDefault::Offered,
        }
    }

    fn group(id: &str, owner: &str) -> SelectionGroup {
        SelectionGroup::import(
            SelectionGroupId::new(id).unwrap(),
            SelectionOwner::Product {
                product_ref: ProductRef::new(owner).unwrap(),
            },
            text(id),
            1,
            1,
            0,
        )
        .unwrap()
    }

    fn option(id: &str, group_id: &str) -> SelectionOption {
        SelectionOption::import(
            SelectionOptionId::new(id).unwrap(),
            SelectionGroupId::new(group_id).unwrap(),
            text(id),
            None,
            1,
            0,
            0,
            None,
        )
        .unwrap()
    }

    /// A Retail catalog keeps the hash it had before the extensions existed,
    /// so a snapshot already activated in a store still matches what an
    /// upgraded producer declares for the same content.
    #[test]
    fn a_catalog_without_extensions_hashes_as_it_always_did() {
        let profiles = vec![profile("IVA16", 1_600)];
        let books = vec![book("BASE", 0)];
        let products = vec![product("P-1")];
        let entries = vec![entry(&books[0], "P-1", 100)];
        let barcodes = vec![barcode("11111", "P-1")];
        let retail = content(&profiles, &books, &products, &entries, &barcodes);

        let mut hasher = ContentHasher::new();
        hasher.begin_tax_profiles();
        hasher.tax_profile(&profiles[0]);
        hasher.begin_price_books();
        hasher.price_book(&books[0]);
        hasher.begin_products();
        hasher.product(&products[0]);
        hasher.begin_price_entries();
        hasher.price_entry(&entries[0]);
        hasher.begin_barcodes();
        hasher.barcode(&barcodes[0]);
        hasher.begin_promotions();

        assert_eq!(hasher.finish(), hash_snapshot(retail));
    }

    #[test]
    fn extensions_hash_independently_of_the_order_they_arrive_in() {
        let products = vec![product("P-1"), product("P-2")];
        let categories = vec![category("C-2", 1), category("C-1", 0)];
        let channels = vec![channel("KIOSK"), channel("COUNTER")];
        let groups = vec![group("G-2", "P-2"), group("G-1", "P-1")];
        let options = vec![
            option("O-2", "G-2"),
            option("O-1", "G-1"),
            option("O-3", "G-1"),
        ];
        let rows = vec![
            ChannelAvailability {
                channel_ref: ChannelRef::new("KIOSK").unwrap(),
                target: AvailabilityTarget::Product {
                    product_ref: ProductRef::new("P-2").unwrap(),
                },
                state: AvailabilityState::Withheld,
            },
            ChannelAvailability {
                channel_ref: ChannelRef::new("COUNTER").unwrap(),
                target: AvailabilityTarget::Category {
                    category_ref: CategoryRef::new("C-1").unwrap(),
                },
                state: AvailabilityState::Offered,
            },
        ];
        let presentation = vec![
            ProductPresentation {
                product_ref: ProductRef::new("P-2").unwrap(),
                category_ref: Some(CategoryRef::new("C-2").unwrap()),
                display_name: None,
                description: None,
                display_order: 1,
                image: None,
                recommended_product_refs: Vec::new(),
            },
            ProductPresentation {
                product_ref: ProductRef::new("P-1").unwrap(),
                category_ref: Some(CategoryRef::new("C-1").unwrap()),
                display_name: None,
                description: None,
                display_order: 0,
                image: None,
                recommended_product_refs: Vec::new(),
            },
        ];

        let forwards = SnapshotContent {
            products: &products,
            channels: &channels,
            categories: &categories,
            presentation: &presentation,
            selection_groups: &groups,
            selection_options: &options,
            availability: &rows,
            ..SnapshotContent::default()
        };
        let reversed_channels: Vec<Channel> = channels.iter().rev().cloned().collect();
        let reversed_categories: Vec<Category> = categories.iter().rev().cloned().collect();
        let reversed_presentation: Vec<ProductPresentation> =
            presentation.iter().rev().cloned().collect();
        let reversed_groups: Vec<SelectionGroup> = groups.iter().rev().cloned().collect();
        let reversed_options: Vec<SelectionOption> = options.iter().rev().cloned().collect();
        let reversed_rows: Vec<ChannelAvailability> = rows.iter().rev().cloned().collect();
        let backwards = SnapshotContent {
            products: &products,
            channels: &reversed_channels,
            categories: &reversed_categories,
            presentation: &reversed_presentation,
            selection_groups: &reversed_groups,
            selection_options: &reversed_options,
            availability: &reversed_rows,
            ..SnapshotContent::default()
        };

        assert_eq!(hash_snapshot(forwards.clone()), hash_snapshot(backwards));
        // And the extensions really do contribute: dropping them changes it.
        assert_ne!(
            hash_snapshot(forwards),
            hash_snapshot(SnapshotContent {
                products: &products,
                ..SnapshotContent::default()
            })
        );
    }

    #[test]
    fn a_translation_is_content() {
        let bare = vec![channel("KIOSK")];
        let mut translated = bare.clone();
        translated[0].name = LocalizedText::with_translations(
            "KIOSK",
            BTreeMap::from([(locale("en"), "Kiosk".to_owned())]),
        )
        .unwrap();

        let hash = |channels: &[Channel]| {
            hash_snapshot(SnapshotContent {
                channels,
                ..SnapshotContent::default()
            })
        };
        assert_ne!(hash(&bare), hash(&translated));

        // ...and the same translations in a different input order are not.
        let one = LocalizedText::with_translations(
            "Refresco",
            BTreeMap::from([
                (locale("en"), "Soda".to_owned()),
                (locale("fr"), "Soda".to_owned()),
            ]),
        )
        .unwrap();
        let other = LocalizedText::with_translations(
            "Refresco",
            BTreeMap::from([
                (locale("fr"), "Soda".to_owned()),
                (locale("en"), "Soda".to_owned()),
            ]),
        )
        .unwrap();
        let mut left = bare.clone();
        left[0].name = one;
        let mut right = bare;
        right[0].name = other;
        assert_eq!(hash(&left), hash(&right));
    }

    #[test]
    fn display_order_and_images_are_content() {
        let hash = |categories: &[Category]| {
            hash_snapshot(SnapshotContent {
                categories,
                ..SnapshotContent::default()
            })
        };
        let plain = vec![category("C-1", 0)];
        let reordered = vec![category("C-1", 7)];
        assert_ne!(hash(&plain), hash(&reordered));

        let mut illustrated = plain.clone();
        illustrated[0].image = Some(
            AssetRef::import(format!("sha256:{}", "a".repeat(64)), "image/webp", 2_048).unwrap(),
        );
        let mut other_photo = plain.clone();
        other_photo[0].image = Some(
            AssetRef::import(format!("sha256:{}", "b".repeat(64)), "image/webp", 2_048).unwrap(),
        );
        assert_ne!(hash(&plain), hash(&illustrated));
        assert_ne!(
            hash(&illustrated),
            hash(&other_photo),
            "a different photo is a different snapshot, so a client may cache by hash"
        );
    }

    #[test]
    fn withholding_a_product_on_one_channel_is_content() {
        let channels = vec![channel("KIOSK")];
        let offered = ChannelAvailability {
            channel_ref: ChannelRef::new("KIOSK").unwrap(),
            target: AvailabilityTarget::Product {
                product_ref: ProductRef::new("P-1").unwrap(),
            },
            state: AvailabilityState::Offered,
        };
        let withheld = ChannelAvailability {
            state: AvailabilityState::Withheld,
            ..offered.clone()
        };
        let snoozed = ChannelAvailability {
            state: AvailabilityState::Snoozed {
                until: Timestamp::parse_rfc3339("2026-10-23T18:00:00Z").unwrap(),
            },
            ..offered.clone()
        };
        let hash = |rows: &[ChannelAvailability]| {
            hash_snapshot(SnapshotContent {
                channels: &channels,
                availability: rows,
                ..SnapshotContent::default()
            })
        };
        let one = std::slice::from_ref;
        assert_ne!(hash(one(&offered)), hash(one(&withheld)));
        assert_ne!(hash(one(&offered)), hash(one(&snoozed)));
        assert_ne!(hash(one(&withheld)), hash(&[]));
    }

    #[test]
    fn selection_quantities_are_content() {
        let hash = |groups: &[SelectionGroup], options: &[SelectionOption]| {
            hash_snapshot(SnapshotContent {
                selection_groups: groups,
                selection_options: options,
                ..SnapshotContent::default()
            })
        };
        let one_of = vec![group("G-1", "P-1")];
        let mut up_to_three = one_of.clone();
        up_to_three[0].max_quantity = 3;
        let mut optional = one_of.clone();
        optional[0].min_quantity = 0;
        let plain = vec![option("O-1", "G-1")];
        let mut defaulted = plain.clone();
        defaulted[0].default_quantity = 1;

        assert_ne!(hash(&one_of, &plain), hash(&up_to_three, &plain));
        assert_ne!(hash(&one_of, &plain), hash(&optional, &plain));
        assert_ne!(hash(&one_of, &plain), hash(&one_of, &defaulted));
    }

    /// Recommendations are content, and an empty list is not content.
    ///
    /// The first half is why the section exists at all; the second is why it
    /// is a section rather than two more fields on `presentation`. A snapshot
    /// activated in a store before recommendations existed has to keep
    /// hashing to the value it was activated with, or an upgraded producer
    /// re-declaring the same menu would look like a different one.
    #[test]
    fn recommendations_change_the_hash_and_an_empty_list_changes_nothing() {
        let products = vec![product("P-1"), product("P-2"), product("P-3")];
        let plain = |product_ref: &str| ProductPresentation {
            product_ref: ProductRef::new(product_ref).unwrap(),
            category_ref: None,
            display_name: None,
            description: None,
            display_order: 0,
            image: None,
            recommended_product_refs: Vec::new(),
        };

        let without = vec![plain("P-1"), plain("P-2")];
        fn content<'a>(
            products: &'a [CatalogProduct],
            presentation: &'a [ProductPresentation],
        ) -> SnapshotContent<'a> {
            SnapshotContent {
                products,
                presentation,
                ..SnapshotContent::default()
            }
        }
        let baseline = hash_snapshot(content(&products, &without));

        let mut with = without.clone();
        with[0].recommended_product_refs = vec![ProductRef::new("P-2").unwrap()];
        assert_ne!(
            baseline,
            hash_snapshot(content(&products, &with)),
            "a menu that cross-sells is not the menu that does not"
        );

        // Order is content too: offering fries first is a different decision.
        let mut pair = without.clone();
        pair[0].recommended_product_refs = vec![
            ProductRef::new("P-2").unwrap(),
            ProductRef::new("P-3").unwrap(),
        ];
        let mut swapped = without.clone();
        swapped[0].recommended_product_refs = vec![
            ProductRef::new("P-3").unwrap(),
            ProductRef::new("P-2").unwrap(),
        ];
        assert_ne!(
            hash_snapshot(content(&products, &pair)),
            hash_snapshot(content(&products, &swapped))
        );

        // Clearing them puts the hash back exactly where it started.
        with[0].recommended_product_refs.clear();
        assert_eq!(baseline, hash_snapshot(content(&products, &with)));
    }

    #[test]
    fn a_chunk_cannot_hash_header_content() {
        // The chunk hash is only ever compared against a replay of the same
        // chunk, so it must cover everything a chunk may carry.
        let products = vec![product("P-1")];
        let options = vec![option("O-1", "G-1")];
        let bare = hash_chunk(ChunkContent {
            products: &products,
            ..ChunkContent::default()
        });
        let with_options = hash_chunk(ChunkContent {
            products: &products,
            selection_options: &options,
            ..ChunkContent::default()
        });
        assert_ne!(bare, with_options);
        assert_eq!(
            bare,
            hash_snapshot(SnapshotContent {
                products: &products,
                ..SnapshotContent::default()
            }),
            "a chunk of products alone hashes like the products alone"
        );
    }
}
