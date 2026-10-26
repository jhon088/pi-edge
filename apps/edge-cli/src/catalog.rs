//! Catalog snapshot importer.
//!
//! A development stand-in for PI.COMMERCE Server: it reads a snapshot file and
//! streams it to the daemon over the Edge Protocol. It computes the same
//! canonical hashes the server will compute, so the core can verify content it
//! never trusted in the first place.
//!
//! The CLI does not open SQLite, and it does not decide anything about the
//! catalog: it transfers content and the core validates, stores, hashes and
//! activates it.

use std::path::Path;

use edge_catalog::{
    hash_chunk, hash_snapshot, CatalogBarcode, CatalogProduct, Category, Channel,
    ChannelAvailability, ChunkContent, Currency, PriceBook, PriceEntry, ProductPresentation,
    Promotion, SelectionGroup, SelectionOption, SnapshotContent, TaxProfile,
};
use edge_client::{EdgeClient, IpcEdgeClient};
use edge_protocol::{
    BeginCatalogImportRequestV1, CatalogProductRecordV1, CatalogSnapshotViewV1, CategoryV1,
    ChannelAvailabilityV1, ChannelV1, CommitCatalogImportRequestV1, PriceBookV1,
    ProductPresentationV1, PromotionRecordV1, PutCatalogChunkRequestV1, SelectionGroupV1,
    SelectionOptionV1, SnapshotCountsV1, TaxProfileV1,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::CliError;

/// Default per-chunk byte budget.
///
/// A client policy, not part of the protocol: chunks are sized by bytes, carry
/// a variable number of records, and stay far below the transport's frame
/// limit.
pub const DEFAULT_CHUNK_BUDGET_BYTES: usize = 256 * 1024;

/// On-disk snapshot format. The shape PI.COMMERCE Server will eventually send.
#[derive(Debug, Deserialize)]
pub struct CatalogFixture {
    /// Monotonic version assigned by the producer.
    pub version: i64,
    /// Currency every price is quoted in.
    pub currency: String,
    pub generated_at: String,
    #[serde(default)]
    pub effective_at: Option<String>,
    pub tax_profiles: Vec<TaxProfileV1>,
    /// A snapshot must declare at least one price book.
    pub price_books: Vec<PriceBookV1>,
    pub products: Vec<CatalogProductRecordV1>,
    /// Promotions the snapshot carries. PI.Edge never creates one.
    #[serde(default)]
    pub promotions: Vec<PromotionRecordV1>,
    /// Universal catalog extensions (ADR 044). Absent in a retail snapshot
    /// file, which is why every one of them defaults.
    #[serde(default)]
    pub channels: Vec<ChannelV1>,
    #[serde(default)]
    pub categories: Vec<CategoryV1>,
    #[serde(default)]
    pub presentation: Vec<ProductPresentationV1>,
    #[serde(default)]
    pub selection_groups: Vec<SelectionGroupV1>,
    #[serde(default)]
    pub selection_options: Vec<SelectionOptionV1>,
    #[serde(default)]
    pub availability: Vec<ChannelAvailabilityV1>,
}

/// One record with its validated domain form, so hashes and payloads agree.
struct PreparedRecord {
    wire: CatalogProductRecordV1,
    product: CatalogProduct,
    prices: Vec<PriceEntry>,
    barcodes: Vec<CatalogBarcode>,
    /// Serialized size, used to fill a chunk by bytes.
    wire_bytes: usize,
}

pub struct ImportOutcome {
    pub snapshot: CatalogSnapshotViewV1,
    pub chunks_sent: i64,
    pub chunks_skipped: i64,
    pub activated: bool,
}

/// Read a snapshot file, stream it to the core, and verify it there.
pub fn import_catalog(
    client: &IpcEdgeClient,
    path: &Path,
    chunk_budget_bytes: usize,
    activate: bool,
) -> Result<ImportOutcome, CliError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| CliError::Input(format!("cannot read {}: {error}", path.display())))?;
    let fixture: CatalogFixture = serde_json::from_str(&raw).map_err(|error| {
        CliError::Input(format!(
            "{} is not a valid snapshot: {error}",
            path.display()
        ))
    })?;

    let currency = Currency::from_code(&fixture.currency)
        .map_err(|error| CliError::Input(format!("unknown currency: {error}")))?;

    let profiles: Vec<TaxProfile> = fixture
        .tax_profiles
        .iter()
        .cloned()
        .map(TaxProfileV1::into_domain)
        .collect::<Result<_, _>>()
        .map_err(|error| CliError::Input(format!("invalid tax profile: {error}")))?;

    let books: Vec<PriceBook> = fixture
        .price_books
        .iter()
        .cloned()
        .map(PriceBookV1::into_domain)
        .collect::<Result<_, _>>()
        .map_err(|error| CliError::Input(format!("invalid price book: {error}")))?;

    // Deterministic order, so a resumed import produces identical chunks.
    let mut records = prepare(fixture.products, currency)?;
    records.sort_by(|left, right| left.product.product_ref.cmp(&right.product.product_ref));

    let all_products: Vec<CatalogProduct> = records
        .iter()
        .map(|record| record.product.clone())
        .collect();
    let all_prices: Vec<PriceEntry> = records
        .iter()
        .flat_map(|record| record.prices.clone())
        .collect();
    let all_barcodes: Vec<CatalogBarcode> = records
        .iter()
        .flat_map(|record| record.barcodes.clone())
        .collect();
    let all_promotions: Vec<Promotion> = fixture
        .promotions
        .iter()
        .cloned()
        .map(|record| record.into_domain(currency))
        .collect::<Result<_, _>>()
        .map_err(|error| CliError::Input(format!("invalid promotion: {error}")))?;

    // The extensions, validated once into their domain form so the hash the
    // CLI declares is computed from exactly what the core will persist.
    let convert = |label: &'static str| {
        move |error: edge_catalog::CatalogError| {
            CliError::Input(format!("invalid {label}: {error}"))
        }
    };
    let all_channels: Vec<Channel> = fixture
        .channels
        .iter()
        .cloned()
        .map(ChannelV1::into_domain)
        .collect::<Result<_, _>>()
        .map_err(convert("channel"))?;
    let all_categories: Vec<Category> = fixture
        .categories
        .iter()
        .cloned()
        .map(CategoryV1::into_domain)
        .collect::<Result<_, _>>()
        .map_err(convert("category"))?;
    let all_presentation: Vec<ProductPresentation> = fixture
        .presentation
        .iter()
        .cloned()
        .map(ProductPresentationV1::into_domain)
        .collect::<Result<_, _>>()
        .map_err(convert("presentation"))?;
    let all_groups: Vec<SelectionGroup> = fixture
        .selection_groups
        .iter()
        .cloned()
        .map(SelectionGroupV1::into_domain)
        .collect::<Result<_, _>>()
        .map_err(convert("selection group"))?;
    let all_options: Vec<SelectionOption> = fixture
        .selection_options
        .iter()
        .cloned()
        .map(SelectionOptionV1::into_domain)
        .collect::<Result<_, _>>()
        .map_err(convert("selection option"))?;
    let all_availability: Vec<ChannelAvailability> = fixture
        .availability
        .iter()
        .cloned()
        .map(ChannelAvailabilityV1::into_domain)
        .collect::<Result<_, _>>()
        .map_err(convert("availability"))?;

    let declared_hash = hash_snapshot(SnapshotContent {
        tax_profiles: &profiles,
        price_books: &books,
        products: &all_products,
        price_entries: &all_prices,
        barcodes: &all_barcodes,
        promotions: &all_promotions,
        channels: &all_channels,
        categories: &all_categories,
        presentation: &all_presentation,
        selection_groups: &all_groups,
        selection_options: &all_options,
        availability: &all_availability,
    });

    let expected = SnapshotCountsV1 {
        tax_profiles: i64::try_from(profiles.len()).unwrap_or(i64::MAX),
        price_books: i64::try_from(books.len()).unwrap_or(i64::MAX),
        products: i64::try_from(all_products.len()).unwrap_or(i64::MAX),
        price_entries: i64::try_from(all_prices.len()).unwrap_or(i64::MAX),
        barcodes: i64::try_from(all_barcodes.len()).unwrap_or(i64::MAX),
        promotions: i64::try_from(all_promotions.len()).unwrap_or(i64::MAX),
        channels: i64::try_from(all_channels.len()).unwrap_or(i64::MAX),
        categories: i64::try_from(all_categories.len()).unwrap_or(i64::MAX),
        presentation: i64::try_from(all_presentation.len()).unwrap_or(i64::MAX),
        selection_groups: i64::try_from(all_groups.len()).unwrap_or(i64::MAX),
        selection_options: i64::try_from(all_options.len()).unwrap_or(i64::MAX),
        availability: i64::try_from(all_availability.len()).unwrap_or(i64::MAX),
    };

    // Opening an import for content that is already open returns its progress,
    // which is what makes a resume an ordinary call.
    let opened = client.begin_catalog_import(
        BeginCatalogImportRequestV1 {
            version: fixture.version,
            currency: fixture.currency.clone(),
            content_hash: declared_hash.clone(),
            generated_at: fixture.generated_at.clone(),
            effective_at: fixture.effective_at.clone(),
            expected,
            tax_profiles: fixture.tax_profiles.clone(),
            price_books: fixture.price_books.clone(),
            channels: fixture.channels.clone(),
            categories: fixture.categories.clone(),
        },
        None,
    )?;
    let snapshot_id = opened.snapshot.snapshot_id.clone();
    let already_received = opened.progress.received_chunks.clone();
    // Content that is already committed or in force needs no transfer at all.
    let import_is_open = opened.snapshot.status == "IMPORTING";

    let chunks = split_by_bytes(&records, chunk_budget_bytes);
    // A promotion that names a product must travel in that product's chunk;
    // one that applies to anything rides in the first chunk.
    let promotion_chunk = assign_promotions(&chunks, &fixture.promotions, &all_promotions);
    let mut chunks_sent = 0;
    let mut chunks_skipped = 0;
    // Presentation, selections and availability reference products, so they
    // ride in the last chunk: by then every product is stored and no reference
    // can dangle. Scheduling them per product would buy nothing — the core
    // validates the whole structure at commit either way.
    let last_chunk = chunks.len().saturating_sub(1);
    for (index, chunk) in chunks.iter().enumerate() {
        if !import_is_open {
            chunks_skipped += 1;
            continue;
        }
        let chunk_index = i64::try_from(index).unwrap_or(i64::MAX);
        if already_received.contains(&chunk_index) {
            chunks_skipped += 1;
            continue;
        }
        let products: Vec<CatalogProduct> =
            chunk.iter().map(|record| record.product.clone()).collect();
        let prices: Vec<PriceEntry> = chunk
            .iter()
            .flat_map(|record| record.prices.clone())
            .collect();
        let barcodes: Vec<CatalogBarcode> = chunk
            .iter()
            .flat_map(|record| record.barcodes.clone())
            .collect();
        let promotions: Vec<Promotion> = promotion_chunk
            .iter()
            .filter(|(assigned, _, _)| *assigned == index)
            .map(|(_, promotion, _)| promotion.clone())
            .collect();
        let tail = index == last_chunk;
        let empty_presentation: Vec<ProductPresentation> = Vec::new();
        let empty_groups: Vec<SelectionGroup> = Vec::new();
        let empty_options: Vec<SelectionOption> = Vec::new();
        let empty_availability: Vec<ChannelAvailability> = Vec::new();
        client.put_catalog_chunk(PutCatalogChunkRequestV1 {
            snapshot_id: snapshot_id.clone(),
            chunk_index,
            chunk_hash: hash_chunk(ChunkContent {
                products: &products,
                price_entries: &prices,
                barcodes: &barcodes,
                promotions: &promotions,
                presentation: if tail {
                    &all_presentation
                } else {
                    &empty_presentation
                },
                selection_groups: if tail { &all_groups } else { &empty_groups },
                selection_options: if tail { &all_options } else { &empty_options },
                availability: if tail {
                    &all_availability
                } else {
                    &empty_availability
                },
            }),
            products: chunk.iter().map(|record| record.wire.clone()).collect(),
            promotions: promotion_chunk
                .iter()
                .filter(|(assigned, _, _)| *assigned == index)
                .map(|(_, _, wire)| wire.clone())
                .collect(),
            presentation: if tail {
                fixture.presentation.clone()
            } else {
                Vec::new()
            },
            selection_groups: if tail {
                fixture.selection_groups.clone()
            } else {
                Vec::new()
            },
            selection_options: if tail {
                fixture.selection_options.clone()
            } else {
                Vec::new()
            },
            availability: if tail {
                fixture.availability.clone()
            } else {
                Vec::new()
            },
        })?;
        chunks_sent += 1;
    }

    let snapshot = client.commit_catalog_import(
        CommitCatalogImportRequestV1 {
            snapshot_id: snapshot_id.clone(),
            chunk_count: i64::try_from(chunks.len()).unwrap_or(i64::MAX),
        },
        None,
    )?;

    let snapshot = if activate {
        client.activate_catalog_snapshot(&snapshot_id, None)?
    } else {
        snapshot
    };

    Ok(ImportOutcome {
        snapshot,
        chunks_sent,
        chunks_skipped,
        activated: activate,
    })
}

impl ImportOutcome {
    pub fn to_value(&self) -> Result<Value, CliError> {
        Ok(json!({
            "snapshot": serde_json::to_value(&self.snapshot)
                .map_err(|e| CliError::Serialization(e.to_string()))?,
            "chunks_sent": self.chunks_sent,
            "chunks_skipped": self.chunks_skipped,
            "activated": self.activated,
        }))
    }
}

fn prepare(
    records: Vec<CatalogProductRecordV1>,
    currency: Currency,
) -> Result<Vec<PreparedRecord>, CliError> {
    records
        .into_iter()
        .map(|wire| {
            let wire_bytes = serde_json::to_vec(&wire)
                .map(|bytes| bytes.len())
                .map_err(|error| CliError::Serialization(error.to_string()))?;
            let parsed = wire
                .clone()
                .into_domain(currency)
                .map_err(|error| CliError::Input(format!("invalid product record: {error}")))?;
            Ok(PreparedRecord {
                wire,
                product: parsed.product,
                prices: parsed.prices,
                barcodes: parsed.barcodes,
                wire_bytes,
            })
        })
        .collect()
}

/// Fill chunks up to a byte budget, never fewer than one record per chunk.
fn split_by_bytes(records: &[PreparedRecord], budget: usize) -> Vec<Vec<&PreparedRecord>> {
    let mut chunks: Vec<Vec<&PreparedRecord>> = Vec::new();
    let mut current: Vec<&PreparedRecord> = Vec::new();
    let mut size = 0;
    for record in records {
        if !current.is_empty() && size + record.wire_bytes > budget {
            chunks.push(std::mem::take(&mut current));
            size = 0;
        }
        size += record.wire_bytes;
        current.push(record);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Decide which chunk each promotion travels in.
///
/// A promotion that names a product has to land in the same chunk — and so the
/// same database transaction — as that product, because the snapshot's foreign
/// key says so. One that applies to any product has nothing to wait for and
/// rides in the first chunk.
fn assign_promotions<'a>(
    chunks: &[Vec<&PreparedRecord>],
    wire: &'a [PromotionRecordV1],
    domain: &'a [Promotion],
) -> Vec<(usize, Promotion, PromotionRecordV1)> {
    let mut assigned = Vec::with_capacity(domain.len());
    for (promotion, record) in domain.iter().zip(wire.iter()) {
        let target = promotion
            .rule
            .applicability()
            .and_then(|applies| applies.product_ref())
            .and_then(|product_ref| {
                chunks.iter().position(|chunk| {
                    chunk
                        .iter()
                        .any(|candidate| &candidate.product.product_ref == product_ref)
                })
            })
            .unwrap_or(0);
        assigned.push((target, promotion.clone(), record.clone()));
    }
    assigned
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    fn record(reference: &str, name_padding: usize) -> CatalogProductRecordV1 {
        CatalogProductRecordV1 {
            product_ref: reference.to_owned(),
            external_ref: None,
            sku: None,
            name: "x".repeat(name_padding),
            active: true,
            sellable: true,
            tax_profile: "IVA16".to_owned(),
            barcodes: vec![format!("bar-{reference}")],
            prices: vec![edge_protocol::PriceRecordV1 {
                price_book: "BASE".to_owned(),
                amount_minor: 100,
                price_mode: "TAX_EXCLUSIVE".to_owned(),
                valid_from: None,
                valid_until: None,
            }],
        }
    }

    #[test]
    fn splits_by_bytes_not_by_record_count() {
        let records = prepare(
            (0..10).map(|i| record(&format!("P-{i:02}"), 100)).collect(),
            Currency::Mxn,
        )
        .unwrap();
        let one_record = records[0].wire_bytes;

        // A budget of two records' worth yields five chunks.
        let chunks = split_by_bytes(&records, one_record * 2);
        assert_eq!(chunks.len(), 5);
        assert!(chunks.iter().all(|chunk| chunk.len() == 2));

        // A generous budget yields one chunk.
        assert_eq!(split_by_bytes(&records, 1024 * 1024).len(), 1);
    }

    #[test]
    fn never_produces_an_empty_chunk_even_for_an_oversized_record() {
        // One record that is already bigger than the whole budget.
        let records = prepare(vec![record("P-1", 400)], Currency::Mxn).unwrap();
        assert!(records[0].wire_bytes > 8);
        let chunks = split_by_bytes(&records, 8);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 1);
    }

    #[test]
    fn chunk_boundaries_are_deterministic_for_the_same_input() {
        let records = prepare(
            (0..7).map(|i| record(&format!("P-{i:02}"), 50)).collect(),
            Currency::Mxn,
        )
        .unwrap();
        let budget = records[0].wire_bytes * 3;
        let first: Vec<usize> = split_by_bytes(&records, budget)
            .iter()
            .map(Vec::len)
            .collect();
        let second: Vec<usize> = split_by_bytes(&records, budget)
            .iter()
            .map(Vec::len)
            .collect();
        assert_eq!(first, second);
    }
}
