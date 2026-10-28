//! SQLite adapter for the catalog context.
//!
//! Lookups on the sale path are index hits by construction: a product by
//! `(snapshot_id, product_ref)` and a barcode by `(snapshot_id, barcode)` are
//! primary keys, and `sku` has its own index. Nothing here scans.
//!
//! The content hash is computed by streaming the stored rows in canonical
//! order through the catalog's hasher, so it describes what was persisted
//! rather than what was received.

use std::collections::BTreeMap;

use edge_application::ports::{CatalogUnitOfWork, StoredChunk};
use edge_application::AppError;
use edge_catalog::channel::{
    AvailabilityDefault, AvailabilityState, AvailabilityTarget, Channel, ChannelAvailability,
};
use edge_catalog::content::ContentHasher;
use edge_catalog::presentation::{AssetRef, Category, Locale, LocalizedText, ProductPresentation};
use edge_catalog::pricing::{NewPriceBook, NewPriceEntry, PriceBookScope};
use edge_catalog::promotions::{
    NewPromotion, ProductApplicability, Promotion, PromotionRule, Stackability,
};
use edge_catalog::selection::{SelectionGroup, SelectionOption, SelectionOwner};
use edge_catalog::snapshot::CatalogSnapshotState;
use edge_catalog::{
    Barcode, CatalogBarcode, CatalogError, CatalogProduct, CatalogSnapshot, CatalogSnapshotStatus,
    ImportProgress, PriceBook, PriceCandidate, PriceEntry, ProductSelector, SnapshotCounts,
    TaxProfile, TaxProfileCode,
};
use edge_domain::prelude::*;
use rusqlite::{params, OptionalExtension, Row};

use crate::error::sqlite_err;
use crate::sqlite::{money, parse_currency, parse_id, parse_opt_ts, parse_ts, SqliteUnitOfWork};

const SNAPSHOT_COLUMNS: &str = "snapshot_id, tenant_id, store_id, version, status, \
     declared_content_hash, content_hash, generated_at, effective_at, expected_tax_profiles, \
     expected_price_books, expected_products, expected_price_entries, expected_barcodes, \
     expected_promotions, imported_at, committed_at, activated_at, superseded_at, \
     expected_channels, expected_categories, expected_presentation, \
     expected_selection_groups, expected_selection_options, expected_availability";

const PRODUCT_COLUMNS: &str = "product_ref, external_ref, sku, name, active, sellable, \
     tax_profile_code";

const PRICE_BOOK_COLUMNS: &str =
    "price_book_id, code, name, currency, scope, scope_store_id, priority, active";

const PRICE_ENTRY_COLUMNS: &str =
    "price_book_id, product_ref, amount_minor, currency, price_mode, valid_from, valid_until";

const PROMOTION_COLUMNS: &str = "promotion_id, code, name, active, priority, stackability, \
     scope, scope_store_id, valid_from, valid_until, rule_kind, applies_to_product_ref, \
     percentage_bps, amount_minor, minimum_minor, currency, buy_thousandths, get_thousandths";

impl CatalogUnitOfWork for SqliteUnitOfWork<'_, '_> {
    fn find_catalog_snapshot(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Option<CatalogSnapshot>, AppError> {
        let raw = self
            .tx
            .query_row(
                &format!("SELECT {SNAPSHOT_COLUMNS} FROM catalog_snapshots WHERE snapshot_id = ?1"),
                params![snapshot_id.to_string()],
                RawSnapshot::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        raw.map(RawSnapshot::into_domain).transpose()
    }

    fn find_catalog_snapshot_by_version(
        &mut self,
        store_id: &StoreId,
        version: i64,
    ) -> Result<Option<CatalogSnapshot>, AppError> {
        let raw = self
            .tx
            .query_row(
                &format!(
                    "SELECT {SNAPSHOT_COLUMNS} FROM catalog_snapshots
                     WHERE store_id = ?1 AND version = ?2"
                ),
                params![store_id.as_str(), version],
                RawSnapshot::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        raw.map(RawSnapshot::into_domain).transpose()
    }

    fn find_active_catalog_snapshot(
        &mut self,
        store_id: &StoreId,
    ) -> Result<Option<CatalogSnapshot>, AppError> {
        let raw = self
            .tx
            .query_row(
                &format!(
                    "SELECT {SNAPSHOT_COLUMNS} FROM catalog_snapshots
                     WHERE store_id = ?1 AND status = 'ACTIVE'"
                ),
                params![store_id.as_str()],
                RawSnapshot::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        raw.map(RawSnapshot::into_domain).transpose()
    }

    fn list_catalog_snapshots(
        &mut self,
        store_id: &StoreId,
        limit: i64,
    ) -> Result<Vec<CatalogSnapshot>, AppError> {
        let mut statement = self
            .tx
            .prepare(&format!(
                "SELECT {SNAPSHOT_COLUMNS} FROM catalog_snapshots
                 WHERE store_id = ?1
                 ORDER BY version DESC
                 LIMIT ?2"
            ))
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![store_id.as_str(), limit], RawSnapshot::from_row)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawSnapshot::into_domain).collect()
    }

    fn highest_catalog_version(&mut self, store_id: &StoreId) -> Result<i64, AppError> {
        // COALESCE, so a store with no catalog answers 0 rather than NULL and
        // the caller has one shape to handle.
        self.tx
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM catalog_snapshots WHERE store_id = ?1",
                params![store_id.as_str()],
                |row| row.get(0),
            )
            .map_err(sqlite_err)
    }

    fn insert_catalog_snapshot(
        &mut self,
        snapshot: &CatalogSnapshot,
        now: Timestamp,
    ) -> Result<(), AppError> {
        let state = snapshot.state();
        self.tx
            .execute(
                "INSERT INTO catalog_snapshots
                     (snapshot_id, tenant_id, store_id, version, status, declared_content_hash,
                      content_hash, generated_at, effective_at, expected_tax_profiles,
                      expected_price_books, expected_products, expected_price_entries,
                      expected_barcodes, expected_promotions, imported_at, committed_at,
                      activated_at, superseded_at, expected_channels, expected_categories,
                      expected_presentation, expected_selection_groups,
                      expected_selection_options, expected_availability, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?21, ?22, ?23, ?24, ?25, ?26, ?20, ?20)",
                params![
                    state.snapshot_id.to_string(),
                    state.tenant_id.as_str(),
                    state.store_id.as_str(),
                    state.version,
                    state.status.as_str(),
                    state.declared_content_hash,
                    state.content_hash,
                    state.generated_at.to_rfc3339(),
                    state.effective_at.map(|ts| ts.to_rfc3339()),
                    state.expected.tax_profiles,
                    state.expected.price_books,
                    state.expected.products,
                    state.expected.price_entries,
                    state.expected.barcodes,
                    state.expected.promotions,
                    state.imported_at.to_rfc3339(),
                    state.committed_at.map(|ts| ts.to_rfc3339()),
                    state.activated_at.map(|ts| ts.to_rfc3339()),
                    state.superseded_at.map(|ts| ts.to_rfc3339()),
                    now.to_rfc3339(),
                    state.expected.channels,
                    state.expected.categories,
                    state.expected.presentation,
                    state.expected.selection_groups,
                    state.expected.selection_options,
                    state.expected.availability,
                ],
            )
            .map_err(|error| {
                if is_constraint_violation(&error) {
                    AppError::Catalog(CatalogError::VersionConflict {
                        version: state.version,
                    })
                } else {
                    sqlite_err(error)
                }
            })?;
        Ok(())
    }

    fn update_catalog_snapshot(
        &mut self,
        snapshot: &CatalogSnapshot,
        now: Timestamp,
    ) -> Result<(), AppError> {
        let state = snapshot.state();
        let affected = self
            .tx
            .execute(
                "UPDATE catalog_snapshots
                 SET status = ?2, content_hash = ?3, committed_at = ?4, activated_at = ?5,
                     superseded_at = ?6, updated_at = ?7
                 WHERE snapshot_id = ?1",
                params![
                    state.snapshot_id.to_string(),
                    state.status.as_str(),
                    state.content_hash,
                    state.committed_at.map(|ts| ts.to_rfc3339()),
                    state.activated_at.map(|ts| ts.to_rfc3339()),
                    state.superseded_at.map(|ts| ts.to_rfc3339()),
                    now.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        if affected != 1 {
            return Err(AppError::ConcurrentModification {
                aggregate: "CatalogSnapshot",
                id: state.snapshot_id.to_string(),
            });
        }
        Ok(())
    }

    fn delete_catalog_snapshot(&mut self, snapshot_id: CatalogSnapshotId) -> Result<(), AppError> {
        // Products, barcodes, tax profiles and chunk records cascade.
        self.tx
            .execute(
                "DELETE FROM catalog_snapshots WHERE snapshot_id = ?1",
                params![snapshot_id.to_string()],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn insert_catalog_tax_profiles(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        profiles: &[TaxProfile],
    ) -> Result<(), AppError> {
        for profile in profiles {
            self.tx
                .execute(
                    "INSERT INTO catalog_tax_profiles (snapshot_id, code, name, rate_bps)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        snapshot_id.to_string(),
                        profile.code.as_str(),
                        profile.name,
                        profile.rate.bps(),
                    ],
                )
                .map_err(|error| {
                    if is_constraint_violation(&error) {
                        AppError::Catalog(CatalogError::InvalidRecord {
                            field: "tax_profile_code",
                            reason: format!("`{}` is declared twice", profile.code),
                        })
                    } else {
                        sqlite_err(error)
                    }
                })?;
        }
        Ok(())
    }

    fn list_catalog_tax_profiles(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<TaxProfile>, AppError> {
        let mut statement = self
            .tx
            .prepare(
                "SELECT code, name, rate_bps FROM catalog_tax_profiles
                 WHERE snapshot_id = ?1
                 ORDER BY code ASC",
            )
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![snapshot_id.to_string()], RawTaxProfile::from_row)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawTaxProfile::into_domain).collect()
    }

    fn find_catalog_tax_profile(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        code: &TaxProfileCode,
    ) -> Result<Option<TaxProfile>, AppError> {
        let raw = self
            .tx
            .query_row(
                "SELECT code, name, rate_bps FROM catalog_tax_profiles
                 WHERE snapshot_id = ?1 AND code = ?2",
                params![snapshot_id.to_string(), code.as_str()],
                RawTaxProfile::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        raw.map(RawTaxProfile::into_domain).transpose()
    }

    fn find_catalog_chunk(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        chunk_index: i64,
    ) -> Result<Option<StoredChunk>, AppError> {
        self.tx
            .query_row(
                "SELECT chunk_index, chunk_hash, product_count, price_entry_count,
                        barcode_count, promotion_count, presentation_count,
                        selection_group_count, selection_option_count, availability_count
                 FROM catalog_import_chunks
                 WHERE snapshot_id = ?1 AND chunk_index = ?2",
                params![snapshot_id.to_string(), chunk_index],
                |row| {
                    Ok(StoredChunk {
                        chunk_index: row.get(0)?,
                        chunk_hash: row.get(1)?,
                        product_count: row.get(2)?,
                        price_entry_count: row.get(3)?,
                        barcode_count: row.get(4)?,
                        promotion_count: row.get(5)?,
                        presentation_count: row.get(6)?,
                        selection_group_count: row.get(7)?,
                        selection_option_count: row.get(8)?,
                        availability_count: row.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(sqlite_err)
    }

    fn record_catalog_chunk(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        chunk: &StoredChunk,
        now: Timestamp,
    ) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO catalog_import_chunks
                     (snapshot_id, chunk_index, chunk_hash, product_count, price_entry_count,
                      barcode_count, promotion_count, presentation_count, selection_group_count,
                      selection_option_count, availability_count, received_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    snapshot_id.to_string(),
                    chunk.chunk_index,
                    chunk.chunk_hash,
                    chunk.product_count,
                    chunk.price_entry_count,
                    chunk.barcode_count,
                    chunk.promotion_count,
                    chunk.presentation_count,
                    chunk.selection_group_count,
                    chunk.selection_option_count,
                    chunk.availability_count,
                    now.to_rfc3339(),
                ],
            )
            .map_err(|error| {
                if is_constraint_violation(&error) {
                    AppError::Catalog(CatalogError::ChunkConflict {
                        index: chunk.chunk_index,
                    })
                } else {
                    sqlite_err(error)
                }
            })?;
        Ok(())
    }

    fn insert_catalog_products(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        products: &[CatalogProduct],
    ) -> Result<(), AppError> {
        for product in products {
            self.tx
                .execute(
                    "INSERT INTO catalog_products
                         (snapshot_id, product_ref, external_ref, sku, name, active, sellable,
                          tax_profile_code)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        snapshot_id.to_string(),
                        product.product_ref.as_str(),
                        product.external_ref,
                        product.sku,
                        product.name,
                        i64::from(product.active),
                        i64::from(product.sellable),
                        product.tax_profile.as_str(),
                    ],
                )
                .map_err(|error| {
                    if is_constraint_violation(&error) {
                        AppError::Catalog(CatalogError::DuplicateProduct {
                            product_ref: product.product_ref.to_string(),
                        })
                    } else {
                        sqlite_err(error)
                    }
                })?;
        }
        Ok(())
    }

    fn insert_catalog_barcodes(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        barcodes: &[CatalogBarcode],
    ) -> Result<(), AppError> {
        for barcode in barcodes {
            self.tx
                .execute(
                    "INSERT INTO catalog_barcodes (snapshot_id, barcode, product_ref)
                     VALUES (?1, ?2, ?3)",
                    params![
                        snapshot_id.to_string(),
                        barcode.barcode.as_str(),
                        barcode.product_ref.as_str(),
                    ],
                )
                .map_err(|error| {
                    if is_foreign_key_violation(&error) {
                        // The barcode names a product that is not in this
                        // snapshot: the chunk is internally inconsistent.
                        AppError::Catalog(CatalogError::InvalidRecord {
                            field: "barcode.product_ref",
                            reason: format!(
                                "`{}` is not a product of this snapshot",
                                barcode.product_ref
                            ),
                        })
                    } else if is_constraint_violation(&error) {
                        AppError::Catalog(CatalogError::DuplicateBarcode {
                            barcode: barcode.barcode.to_string(),
                        })
                    } else {
                        sqlite_err(error)
                    }
                })?;
        }
        Ok(())
    }

    fn insert_catalog_price_books(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        books: &[PriceBook],
    ) -> Result<(), AppError> {
        for book in books {
            self.tx
                .execute(
                    "INSERT INTO price_books
                         (snapshot_id, price_book_id, code, name, currency, scope,
                          scope_store_id, priority, active)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        snapshot_id.to_string(),
                        book.price_book_id.as_str(),
                        book.code,
                        book.name,
                        book.currency.code(),
                        book.scope.as_str(),
                        book.scope.store_id().map(StoreId::as_str),
                        book.priority,
                        i64::from(book.active),
                    ],
                )
                .map_err(|error| {
                    if is_constraint_violation(&error) {
                        AppError::Catalog(CatalogError::InvalidRecord {
                            field: "price_book_id",
                            reason: format!("`{}` is declared twice", book.price_book_id),
                        })
                    } else {
                        sqlite_err(error)
                    }
                })?;
        }
        Ok(())
    }

    fn list_catalog_price_books(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<PriceBook>, AppError> {
        let mut statement = self
            .tx
            .prepare(&format!(
                "SELECT {PRICE_BOOK_COLUMNS} FROM price_books
                 WHERE snapshot_id = ?1
                 ORDER BY price_book_id ASC"
            ))
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![snapshot_id.to_string()], RawPriceBook::from_row)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawPriceBook::into_domain).collect()
    }

    fn insert_catalog_price_entries(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        entries: &[PriceEntry],
    ) -> Result<(), AppError> {
        for entry in entries {
            self.tx
                .execute(
                    "INSERT INTO price_entries
                         (snapshot_id, price_book_id, product_ref, amount_minor, currency,
                          price_mode, valid_from, valid_until)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        snapshot_id.to_string(),
                        entry.price_book_id.as_str(),
                        entry.product_ref.as_str(),
                        entry.amount.minor(),
                        entry.amount.currency().code(),
                        entry.price_mode.as_str(),
                        entry.valid_from.map(|ts| ts.to_rfc3339()),
                        entry.valid_until.map(|ts| ts.to_rfc3339()),
                    ],
                )
                .map_err(|error| {
                    if is_foreign_key_violation(&error) {
                        // Either the book or the product is not in this
                        // snapshot: the chunk is internally inconsistent.
                        AppError::Catalog(CatalogError::UnknownPriceBook {
                            price_book_id: entry.price_book_id.to_string(),
                        })
                    } else if is_constraint_violation(&error) {
                        AppError::Catalog(CatalogError::DuplicatePriceEntry {
                            price_book_id: entry.price_book_id.to_string(),
                            product_ref: entry.product_ref.to_string(),
                        })
                    } else {
                        sqlite_err(error)
                    }
                })?;
        }
        Ok(())
    }

    fn price_candidates(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        store_id: &StoreId,
        product_ref: &ProductRef,
    ) -> Result<Vec<PriceCandidate>, AppError> {
        let mut statement = self
            .tx
            .prepare(&format!(
                "SELECT {}, {}
                 FROM price_entries e
                 JOIN price_books b
                   ON b.snapshot_id = e.snapshot_id AND b.price_book_id = e.price_book_id
                 WHERE e.snapshot_id = ?1
                   AND e.product_ref = ?2
                   AND b.active = 1
                   AND (b.scope = 'TENANT' OR b.scope_store_id = ?3)
                 ORDER BY b.price_book_id ASC, IFNULL(e.valid_from, '') ASC",
                PRICE_BOOK_COLUMNS
                    .split(", ")
                    .map(|column| format!("b.{column}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                PRICE_ENTRY_COLUMNS
                    .split(", ")
                    .map(|column| format!("e.{column}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(
                params![
                    snapshot_id.to_string(),
                    product_ref.as_str(),
                    store_id.as_str()
                ],
                |row| {
                    Ok((
                        RawPriceBook::from_row(row)?,
                        RawPriceEntry::from_offset(row, 8)?,
                    ))
                },
            )
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter()
            .map(|(book, entry)| {
                Ok(PriceCandidate {
                    book: book.into_domain()?,
                    entry: entry.into_domain()?,
                })
            })
            .collect()
    }

    fn insert_catalog_promotions(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        promotions: &[Promotion],
    ) -> Result<(), AppError> {
        for promotion in promotions {
            let rule = RuleColumns::from(&promotion.rule);
            self.tx
                .execute(
                    "INSERT INTO promotions
                         (snapshot_id, promotion_id, code, name, active, priority, stackability,
                          scope, scope_store_id, valid_from, valid_until, rule_kind,
                          applies_to_product_ref, percentage_bps, amount_minor, minimum_minor,
                          currency, buy_thousandths, get_thousandths)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                             ?16, ?17, ?18, ?19)",
                    params![
                        snapshot_id.to_string(),
                        promotion.promotion_id.as_str(),
                        promotion.code,
                        promotion.name,
                        i64::from(promotion.active),
                        promotion.priority,
                        promotion.stackability.as_str(),
                        promotion.scope.as_str(),
                        promotion.scope.store_id().map(StoreId::as_str),
                        promotion.valid_from.map(|ts| ts.to_rfc3339()),
                        promotion.valid_until.map(|ts| ts.to_rfc3339()),
                        promotion.rule.kind(),
                        rule.product_ref,
                        rule.percentage_bps,
                        rule.amount_minor,
                        rule.minimum_minor,
                        rule.currency,
                        rule.buy_thousandths,
                        rule.get_thousandths,
                    ],
                )
                .map_err(|error| {
                    if is_constraint_violation(&error) {
                        AppError::Catalog(CatalogError::InvalidRecord {
                            field: "promotion_id",
                            reason: format!("`{}` is declared twice", promotion.promotion_id),
                        })
                    } else {
                        sqlite_err(error)
                    }
                })?;
        }
        Ok(())
    }

    fn list_catalog_promotions(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<Promotion>, AppError> {
        let mut statement = self
            .tx
            .prepare(&format!(
                "SELECT {PROMOTION_COLUMNS} FROM promotions
                 WHERE snapshot_id = ?1
                 ORDER BY promotion_id ASC"
            ))
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![snapshot_id.to_string()], RawPromotion::from_row)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawPromotion::into_domain).collect()
    }

    fn promotion_candidates(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        store_id: &StoreId,
    ) -> Result<Vec<Promotion>, AppError> {
        let mut statement = self
            .tx
            .prepare(&format!(
                "SELECT {PROMOTION_COLUMNS} FROM promotions
                 WHERE snapshot_id = ?1
                   AND active = 1
                   AND (scope = 'TENANT' OR scope_store_id = ?2)
                 ORDER BY priority DESC, promotion_id ASC"
            ))
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(
                params![snapshot_id.to_string(), store_id.as_str()],
                RawPromotion::from_row,
            )
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawPromotion::into_domain).collect()
    }

    fn catalog_stored_counts(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<SnapshotCounts, AppError> {
        let id = snapshot_id.to_string();
        Ok(SnapshotCounts {
            tax_profiles: self.count("catalog_tax_profiles", &id)?,
            price_books: self.count("price_books", &id)?,
            products: self.count("catalog_products", &id)?,
            price_entries: self.count("price_entries", &id)?,
            barcodes: self.count("catalog_barcodes", &id)?,
            promotions: self.count("promotions", &id)?,
            channels: self.count("catalog_channels", &id)?,
            categories: self.count("catalog_categories", &id)?,
            presentation: self.count("catalog_product_presentation", &id)?,
            selection_groups: self.count("catalog_selection_groups", &id)?,
            selection_options: self.count("catalog_selection_options", &id)?,
            availability: self.count("catalog_channel_availability", &id)?,
        })
    }

    fn catalog_import_progress(
        &mut self,
        snapshot: &CatalogSnapshot,
    ) -> Result<ImportProgress, AppError> {
        let id = snapshot.id().to_string();
        let mut statement = self
            .tx
            .prepare(
                "SELECT chunk_index, product_count FROM catalog_import_chunks
                 WHERE snapshot_id = ?1
                 ORDER BY chunk_index ASC",
            )
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;

        let mut received_chunks = Vec::with_capacity(rows.len());
        let mut received_records = 0;
        for (index, products) in rows {
            received_chunks.push(index);
            received_records += products;
        }
        Ok(ImportProgress {
            snapshot_id: snapshot.id(),
            status: snapshot.status(),
            version: snapshot.version(),
            received_chunks,
            received_records,
            expected: snapshot.state().expected,
            stored: self.catalog_stored_counts(snapshot.id())?,
        })
    }

    fn lookup_catalog_product(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        selector: &ProductSelector,
    ) -> Result<Option<CatalogProduct>, AppError> {
        let id = snapshot_id.to_string();
        let raw = match selector {
            ProductSelector::Barcode(barcode) => self
                .tx
                .query_row(
                    &format!(
                        "SELECT {PRODUCT_COLUMNS} FROM catalog_products
                         WHERE snapshot_id = ?1 AND product_ref = (
                             SELECT product_ref FROM catalog_barcodes
                             WHERE snapshot_id = ?1 AND barcode = ?2
                         )"
                    ),
                    params![id, barcode.as_str()],
                    RawProduct::from_row,
                )
                .optional(),
            ProductSelector::Sku(sku) => self
                .tx
                .query_row(
                    &format!(
                        "SELECT {PRODUCT_COLUMNS} FROM catalog_products
                         WHERE snapshot_id = ?1 AND sku = ?2
                         LIMIT 1"
                    ),
                    params![id, sku],
                    RawProduct::from_row,
                )
                .optional(),
            ProductSelector::ProductRef(product_ref) => self
                .tx
                .query_row(
                    &format!(
                        "SELECT {PRODUCT_COLUMNS} FROM catalog_products
                         WHERE snapshot_id = ?1 AND product_ref = ?2"
                    ),
                    params![id, product_ref.as_str()],
                    RawProduct::from_row,
                )
                .optional(),
        }
        .map_err(sqlite_err)?;
        raw.map(RawProduct::into_domain).transpose()
    }

    // ---- universal catalog extensions (ADR 044) ----

    fn insert_catalog_channels(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        channels: &[Channel],
    ) -> Result<(), AppError> {
        let id = snapshot_id.to_string();
        for channel in channels {
            self.tx
                .execute(
                    "INSERT INTO catalog_channels
                         (snapshot_id, channel_ref, name_default, default_availability)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        id,
                        channel.channel_ref.as_str(),
                        channel.name.default,
                        channel.default_availability.as_str(),
                    ],
                )
                .map_err(|error| {
                    if is_constraint_violation(&error) {
                        AppError::Catalog(CatalogError::DuplicateChannel {
                            channel_ref: channel.channel_ref.to_string(),
                        })
                    } else {
                        sqlite_err(error)
                    }
                })?;
            self.insert_translations(
                &id,
                TextOwner::Channel,
                channel.channel_ref.as_str(),
                TextField::Name,
                &channel.name,
            )?;
        }
        Ok(())
    }

    fn list_catalog_channels(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<Channel>, AppError> {
        let id = snapshot_id.to_string();
        let mut translations = self.load_translations(&id, TextOwner::Channel)?;
        let mut statement = self
            .tx
            .prepare(
                "SELECT channel_ref, name_default, default_availability FROM catalog_channels
                 WHERE snapshot_id = ?1
                 ORDER BY channel_ref ASC",
            )
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;

        rows.into_iter()
            .map(|(channel_ref, name, default_availability)| {
                Ok(Channel {
                    channel_ref: parse_id(&channel_ref)?,
                    name: localized(&mut translations, &channel_ref, TextField::Name, name)?,
                    default_availability: AvailabilityDefault::parse(&default_availability)
                        .map_err(AppError::Catalog)?,
                })
            })
            .collect()
    }

    fn insert_catalog_categories(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        categories: &[Category],
    ) -> Result<(), AppError> {
        let id = snapshot_id.to_string();
        for category in categories {
            let image = AssetColumns::from(category.image.as_ref());
            self.tx
                .execute(
                    "INSERT INTO catalog_categories
                         (snapshot_id, category_ref, name_default, display_order,
                          image_content_hash, image_media_type, image_byte_size)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        id,
                        category.category_ref.as_str(),
                        category.name.default,
                        category.display_order,
                        image.content_hash,
                        image.media_type,
                        image.byte_size,
                    ],
                )
                .map_err(|error| {
                    if is_constraint_violation(&error) {
                        AppError::Catalog(CatalogError::DuplicateCategory {
                            category_ref: category.category_ref.to_string(),
                        })
                    } else {
                        sqlite_err(error)
                    }
                })?;
            self.insert_translations(
                &id,
                TextOwner::Category,
                category.category_ref.as_str(),
                TextField::Name,
                &category.name,
            )?;
        }
        Ok(())
    }

    fn list_catalog_categories(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<Category>, AppError> {
        let id = snapshot_id.to_string();
        let mut translations = self.load_translations(&id, TextOwner::Category)?;
        let mut statement = self
            .tx
            .prepare(
                "SELECT category_ref, name_default, display_order, image_content_hash,
                        image_media_type, image_byte_size
                 FROM catalog_categories
                 WHERE snapshot_id = ?1
                 ORDER BY category_ref ASC",
            )
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                    AssetColumns::from_row(row, 3)?,
                ))
            })
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;

        rows.into_iter()
            .map(|(category_ref, name, display_order, image)| {
                Ok(Category::import(
                    parse_id(&category_ref)?,
                    localized(&mut translations, &category_ref, TextField::Name, name)?,
                    display_order,
                    image.into_domain()?,
                ))
            })
            .collect()
    }

    fn insert_catalog_presentation(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        presentation: &[ProductPresentation],
    ) -> Result<(), AppError> {
        let id = snapshot_id.to_string();
        for row in presentation {
            let image = AssetColumns::from(row.image.as_ref());
            self.tx
                .execute(
                    "INSERT INTO catalog_product_presentation
                         (snapshot_id, product_ref, category_ref, display_name_default,
                          description_default, display_order, image_content_hash,
                          image_media_type, image_byte_size)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        id,
                        row.product_ref.as_str(),
                        row.category_ref.as_ref().map(CategoryRef::as_str),
                        row.display_name.as_ref().map(|text| text.default.clone()),
                        row.description.as_ref().map(|text| text.default.clone()),
                        row.display_order,
                        image.content_hash,
                        image.media_type,
                        image.byte_size,
                    ],
                )
                .map_err(|error| {
                    if is_foreign_key_violation(&error) {
                        // Either the product or the category is absent. The
                        // producer is told which row to fix; which of its two
                        // references is wrong is in the row itself.
                        AppError::Catalog(CatalogError::PresentationWithoutProduct {
                            product_ref: row.product_ref.to_string(),
                        })
                    } else if is_constraint_violation(&error) {
                        AppError::Catalog(CatalogError::DuplicatePresentation {
                            product_ref: row.product_ref.to_string(),
                        })
                    } else {
                        sqlite_err(error)
                    }
                })?;
            if let Some(text) = &row.display_name {
                self.insert_translations(
                    &id,
                    TextOwner::Product,
                    row.product_ref.as_str(),
                    TextField::DisplayName,
                    text,
                )?;
            }
            if let Some(text) = &row.description {
                self.insert_translations(
                    &id,
                    TextOwner::Product,
                    row.product_ref.as_str(),
                    TextField::Description,
                    text,
                )?;
            }
            self.insert_catalog_recommendations(&id, row)?;
        }
        Ok(())
    }

    fn list_catalog_presentation(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<ProductPresentation>, AppError> {
        let id = snapshot_id.to_string();
        let mut translations = self.load_translations(&id, TextOwner::Product)?;
        let mut recommendations = self.load_recommendations(&id)?;
        let mut statement = self
            .tx
            .prepare(
                "SELECT product_ref, category_ref, display_name_default, description_default,
                        display_order, image_content_hash, image_media_type, image_byte_size
                 FROM catalog_product_presentation
                 WHERE snapshot_id = ?1
                 ORDER BY product_ref ASC",
            )
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i32>(4)?,
                    AssetColumns::from_row(row, 5)?,
                ))
            })
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;

        rows.into_iter()
            .map(
                |(product_ref, category_ref, display_name, description, display_order, image)| {
                    Ok(ProductPresentation {
                        product_ref: parse_id(&product_ref)?,
                        category_ref: category_ref.as_deref().map(parse_id).transpose()?,
                        display_name: display_name
                            .map(|value| {
                                localized(
                                    &mut translations,
                                    &product_ref,
                                    TextField::DisplayName,
                                    value,
                                )
                            })
                            .transpose()?,
                        description: description
                            .map(|value| {
                                localized(
                                    &mut translations,
                                    &product_ref,
                                    TextField::Description,
                                    value,
                                )
                            })
                            .transpose()?,
                        display_order,
                        image: image.into_domain()?,
                        recommended_product_refs: recommendations
                            .remove(&product_ref)
                            .unwrap_or_default(),
                    })
                },
            )
            .collect()
    }

    fn insert_catalog_selection_groups(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        groups: &[SelectionGroup],
    ) -> Result<(), AppError> {
        let id = snapshot_id.to_string();
        for group in groups {
            let (owner_kind, owner_product, owner_option) = match &group.owner {
                SelectionOwner::Product { product_ref } => {
                    ("PRODUCT", Some(product_ref.as_str()), None)
                }
                SelectionOwner::Option { option_id } => ("OPTION", None, Some(option_id.as_str())),
            };
            self.tx
                .execute(
                    "INSERT INTO catalog_selection_groups
                         (snapshot_id, group_id, owner_kind, owner_product_ref, owner_option_id,
                          name_default, min_quantity, max_quantity, display_order)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        id,
                        group.group_id.as_str(),
                        owner_kind,
                        owner_product,
                        owner_option,
                        group.name.default,
                        group.min_quantity,
                        group.max_quantity,
                        group.display_order,
                    ],
                )
                .map_err(|error| {
                    if is_foreign_key_violation(&error) {
                        AppError::Catalog(CatalogError::UnknownSelectionOwner {
                            group_id: group.group_id.to_string(),
                            owner: owner_product.unwrap_or_default().to_owned(),
                        })
                    } else if is_constraint_violation(&error) {
                        AppError::Catalog(CatalogError::DuplicateSelectionGroup {
                            group_id: group.group_id.to_string(),
                        })
                    } else {
                        sqlite_err(error)
                    }
                })?;
            self.insert_translations(
                &id,
                TextOwner::SelectionGroup,
                group.group_id.as_str(),
                TextField::Name,
                &group.name,
            )?;
        }
        Ok(())
    }

    fn insert_catalog_selection_options(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        options: &[SelectionOption],
    ) -> Result<(), AppError> {
        let id = snapshot_id.to_string();
        for option in options {
            let image = AssetColumns::from(option.image.as_ref());
            self.tx
                .execute(
                    "INSERT INTO catalog_selection_options
                         (snapshot_id, option_id, group_id, name_default, component_product_ref,
                          max_quantity, default_quantity, display_order, image_content_hash,
                          image_media_type, image_byte_size)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![
                        id,
                        option.option_id.as_str(),
                        option.group_id.as_str(),
                        option.name.default,
                        option.component_ref.as_ref().map(ProductRef::as_str),
                        option.max_quantity,
                        option.default_quantity,
                        option.display_order,
                        image.content_hash,
                        image.media_type,
                        image.byte_size,
                    ],
                )
                .map_err(|error| {
                    if is_foreign_key_violation(&error) {
                        // The group, or the product a chargeable option bills
                        // through. Both are refusals of the same kind: an
                        // option cannot reference what the snapshot lacks.
                        AppError::Catalog(CatalogError::UnknownSelectionComponent {
                            option_id: option.option_id.to_string(),
                            product_ref: option
                                .component_ref
                                .as_ref()
                                .map(ProductRef::to_string)
                                .unwrap_or_else(|| option.group_id.to_string()),
                        })
                    } else if is_constraint_violation(&error) {
                        AppError::Catalog(CatalogError::DuplicateSelectionOption {
                            option_id: option.option_id.to_string(),
                        })
                    } else {
                        sqlite_err(error)
                    }
                })?;
            self.insert_translations(
                &id,
                TextOwner::SelectionOption,
                option.option_id.as_str(),
                TextField::Name,
                &option.name,
            )?;
        }
        Ok(())
    }

    fn list_catalog_selection_groups(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<SelectionGroup>, AppError> {
        let id = snapshot_id.to_string();
        let mut translations = self.load_translations(&id, TextOwner::SelectionGroup)?;
        let mut statement = self
            .tx
            .prepare(
                "SELECT group_id, owner_kind, owner_product_ref, owner_option_id, name_default,
                        min_quantity, max_quantity, display_order
                 FROM catalog_selection_groups
                 WHERE snapshot_id = ?1
                 ORDER BY group_id ASC",
            )
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i32>(7)?,
                ))
            })
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;

        rows.into_iter()
            .map(
                |(group_id, owner_kind, owner_product, owner_option, name, min, max, order)| {
                    let owner = match (owner_kind.as_str(), owner_product, owner_option) {
                        ("PRODUCT", Some(product_ref), _) => SelectionOwner::Product {
                            product_ref: parse_id(&product_ref)?,
                        },
                        ("OPTION", _, Some(option_id)) => SelectionOwner::Option {
                            option_id: parse_id(&option_id)?,
                        },
                        _ => {
                            return Err(AppError::Catalog(CatalogError::InvalidRecord {
                                field: "selection_group.owner",
                                reason: format!("`{owner_kind}` has no owner reference"),
                            }))
                        }
                    };
                    SelectionGroup::import(
                        parse_id(&group_id)?,
                        owner,
                        localized(&mut translations, &group_id, TextField::Name, name)?,
                        min,
                        max,
                        order,
                    )
                    .map_err(AppError::Catalog)
                },
            )
            .collect()
    }

    fn list_catalog_selection_options(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<SelectionOption>, AppError> {
        let id = snapshot_id.to_string();
        let mut translations = self.load_translations(&id, TextOwner::SelectionOption)?;
        let mut statement = self
            .tx
            .prepare(
                "SELECT option_id, group_id, name_default, component_product_ref, max_quantity,
                        default_quantity, display_order, image_content_hash, image_media_type,
                        image_byte_size
                 FROM catalog_selection_options
                 WHERE snapshot_id = ?1
                 ORDER BY group_id ASC, option_id ASC",
            )
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i32>(6)?,
                    AssetColumns::from_row(row, 7)?,
                ))
            })
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;

        rows.into_iter()
            .map(
                |(option_id, group_id, name, component, max, default, order, image)| {
                    SelectionOption::import(
                        parse_id(&option_id)?,
                        parse_id(&group_id)?,
                        localized(&mut translations, &option_id, TextField::Name, name)?,
                        component.as_deref().map(parse_id).transpose()?,
                        max,
                        default,
                        order,
                        image.into_domain()?,
                    )
                    .map_err(AppError::Catalog)
                },
            )
            .collect()
    }

    fn insert_catalog_availability(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        rows: &[ChannelAvailability],
    ) -> Result<(), AppError> {
        let id = snapshot_id.to_string();
        for row in rows {
            self.tx
                .execute(
                    "INSERT INTO catalog_channel_availability
                         (snapshot_id, channel_ref, target_kind, target_ref, state, snoozed_until)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        id,
                        row.channel_ref.as_str(),
                        row.target.as_str(),
                        row.target.reference(),
                        row.state.as_str(),
                        row.state.snoozed_until().map(|until| until.to_rfc3339()),
                    ],
                )
                .map_err(|error| {
                    if is_foreign_key_violation(&error) {
                        AppError::Catalog(CatalogError::UnknownChannel {
                            channel_ref: row.channel_ref.to_string(),
                        })
                    } else if is_constraint_violation(&error) {
                        AppError::Catalog(CatalogError::DuplicateAvailability {
                            channel_ref: row.channel_ref.to_string(),
                            target: format!("{}:{}", row.target.as_str(), row.target.reference()),
                        })
                    } else {
                        sqlite_err(error)
                    }
                })?;
        }
        Ok(())
    }

    fn list_catalog_availability(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<ChannelAvailability>, AppError> {
        let mut statement = self
            .tx
            .prepare(
                "SELECT channel_ref, target_kind, target_ref, state, snoozed_until
                 FROM catalog_channel_availability
                 WHERE snapshot_id = ?1
                 ORDER BY channel_ref ASC, target_kind ASC, target_ref ASC",
            )
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![snapshot_id.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;

        rows.into_iter()
            .map(|(channel_ref, kind, reference, state, until)| {
                let target = match kind.as_str() {
                    "PRODUCT" => AvailabilityTarget::Product {
                        product_ref: parse_id(&reference)?,
                    },
                    "CATEGORY" => AvailabilityTarget::Category {
                        category_ref: parse_id(&reference)?,
                    },
                    "SELECTION_OPTION" => AvailabilityTarget::SelectionOption {
                        option_id: parse_id(&reference)?,
                    },
                    other => {
                        return Err(AppError::Catalog(CatalogError::InvalidRecord {
                            field: "availability.target_kind",
                            reason: format!("`{other}` is not an availability target"),
                        }))
                    }
                };
                let state = match state.as_str() {
                    "OFFERED" => AvailabilityState::Offered,
                    "WITHHELD" => AvailabilityState::Withheld,
                    "SNOOZED" => AvailabilityState::Snoozed {
                        until: parse_opt_ts(until)?.ok_or(AppError::Catalog(
                            CatalogError::InvalidRecord {
                                field: "availability.snoozed_until",
                                reason: "a snooze must say when it ends".to_owned(),
                            },
                        ))?,
                    },
                    other => {
                        return Err(AppError::Catalog(CatalogError::InvalidRecord {
                            field: "availability.state",
                            reason: format!("`{other}` is not an availability state"),
                        }))
                    }
                };
                Ok(ChannelAvailability {
                    channel_ref: parse_id(&channel_ref)?,
                    target,
                    state,
                })
            })
            .collect()
    }

    fn list_catalog_extension_product_refs(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<ProductRef>, AppError> {
        let mut statement = self
            .tx
            .prepare(
                "SELECT product_ref FROM catalog_products
                 WHERE snapshot_id = ?1
                   AND (product_ref IN (SELECT owner_product_ref FROM catalog_selection_groups
                                        WHERE snapshot_id = ?1)
                     OR product_ref IN (SELECT component_product_ref
                                        FROM catalog_selection_options
                                        WHERE snapshot_id = ?1)
                     OR product_ref IN (SELECT target_ref FROM catalog_channel_availability
                                        WHERE snapshot_id = ?1 AND target_kind = 'PRODUCT'))
                 ORDER BY product_ref ASC",
            )
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![snapshot_id.to_string()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.iter().map(|value| parse_id(value)).collect()
    }

    fn list_catalog_required_assets(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<AssetRef>, AppError> {
        // Derived from the content rows themselves, so the manifest cannot
        // disagree with the snapshot it describes (ADR 045). `DISTINCT` is
        // what makes one photo shared by ten products one entry.
        let mut statement = self
            .tx
            .prepare(
                "SELECT DISTINCT image_content_hash, image_media_type, image_byte_size
                 FROM (
                     SELECT image_content_hash, image_media_type, image_byte_size
                     FROM catalog_categories WHERE snapshot_id = ?1
                     UNION ALL
                     SELECT image_content_hash, image_media_type, image_byte_size
                     FROM catalog_product_presentation WHERE snapshot_id = ?1
                     UNION ALL
                     SELECT image_content_hash, image_media_type, image_byte_size
                     FROM catalog_selection_options WHERE snapshot_id = ?1
                 )
                 WHERE image_content_hash IS NOT NULL
                 ORDER BY image_content_hash ASC, image_media_type ASC, image_byte_size ASC",
            )
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![snapshot_id.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;

        rows.into_iter()
            .map(|(content_hash, media_type, byte_size)| {
                AssetRef::import(content_hash, media_type, byte_size).map_err(AppError::Catalog)
            })
            .collect()
    }

    fn compute_catalog_content_hash(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<String, AppError> {
        let id = snapshot_id.to_string();
        let mut hasher = ContentHasher::new();

        hasher.begin_tax_profiles();
        for profile in self.list_catalog_tax_profiles(snapshot_id)? {
            hasher.tax_profile(&profile);
        }

        hasher.begin_price_books();
        for book in self.list_catalog_price_books(snapshot_id)? {
            hasher.price_book(&book);
        }

        hasher.begin_products();
        {
            let mut statement = self
                .tx
                .prepare(&format!(
                    "SELECT {PRODUCT_COLUMNS} FROM catalog_products
                     WHERE snapshot_id = ?1
                     ORDER BY product_ref ASC"
                ))
                .map_err(sqlite_err)?;
            let mut rows = statement.query(params![id]).map_err(sqlite_err)?;
            while let Some(row) = rows.next().map_err(sqlite_err)? {
                let product = RawProduct::from_row(row)
                    .map_err(sqlite_err)?
                    .into_domain()?;
                hasher.product(&product);
            }
        }

        hasher.begin_price_entries();
        {
            let mut statement = self
                .tx
                .prepare(&format!(
                    "SELECT {PRICE_ENTRY_COLUMNS} FROM price_entries
                     WHERE snapshot_id = ?1
                     ORDER BY price_book_id ASC, product_ref ASC, IFNULL(valid_from, '') ASC"
                ))
                .map_err(sqlite_err)?;
            let mut rows = statement.query(params![id]).map_err(sqlite_err)?;
            while let Some(row) = rows.next().map_err(sqlite_err)? {
                let entry = RawPriceEntry::from_row(row)
                    .map_err(sqlite_err)?
                    .into_domain()?;
                hasher.price_entry(&entry);
            }
        }

        hasher.begin_barcodes();
        {
            let mut statement = self
                .tx
                .prepare(
                    "SELECT barcode, product_ref FROM catalog_barcodes
                     WHERE snapshot_id = ?1
                     ORDER BY barcode ASC",
                )
                .map_err(sqlite_err)?;
            let mut rows = statement.query(params![id]).map_err(sqlite_err)?;
            while let Some(row) = rows.next().map_err(sqlite_err)? {
                let barcode = CatalogBarcode {
                    barcode: Barcode::new(row.get::<_, String>(0).map_err(sqlite_err)?)
                        .map_err(AppError::Catalog)?,
                    product_ref: parse_id(&row.get::<_, String>(1).map_err(sqlite_err)?)?,
                };
                hasher.barcode(&barcode);
            }
        }

        hasher.begin_promotions();
        for promotion in self.list_catalog_promotions(snapshot_id)? {
            hasher.promotion(&promotion);
        }

        // Sections 7 to 12 are skipped when empty, exactly as
        // `hash_snapshot` skips them, so a Retail snapshot still hashes to
        // what it hashed before the extensions existed (ADR 044).
        let channels = self.list_catalog_channels(snapshot_id)?;
        if !channels.is_empty() {
            hasher.begin_channels();
            for channel in &channels {
                hasher.channel(channel);
            }
        }
        let categories = self.list_catalog_categories(snapshot_id)?;
        if !categories.is_empty() {
            hasher.begin_categories();
            for category in &categories {
                hasher.category(category);
            }
        }
        let presentation = self.list_catalog_presentation(snapshot_id)?;
        if !presentation.is_empty() {
            hasher.begin_presentation();
            for row in &presentation {
                hasher.presentation(row);
            }
        }
        let groups = self.list_catalog_selection_groups(snapshot_id)?;
        if !groups.is_empty() {
            hasher.begin_selection_groups();
            for group in &groups {
                hasher.selection_group(group);
            }
        }
        let options = self.list_catalog_selection_options(snapshot_id)?;
        if !options.is_empty() {
            hasher.begin_selection_options();
            for option in &options {
                hasher.selection_option(option);
            }
        }
        let availability = self.list_catalog_availability(snapshot_id)?;
        if !availability.is_empty() {
            hasher.begin_availability();
            for row in &availability {
                hasher.availability(row);
            }
        }
        // Recommendations ride on the presentation rows already read, and
        // contribute nothing when nobody recommends anything — so a snapshot
        // that predates them keeps the hash it was activated with.
        let recommending: Vec<&ProductPresentation> = presentation
            .iter()
            .filter(|row| !row.recommended_product_refs.is_empty())
            .collect();
        if !recommending.is_empty() {
            hasher.begin_recommendations();
            for row in recommending {
                hasher.recommendations(row);
            }
        }

        Ok(hasher.finish())
    }
}

/// Which kind of row a localized string belongs to.
///
/// The translations table is shared by every localizable field, so the owner
/// kind is what keeps a category called `BURGERS` and a channel called
/// `BURGERS` from reading each other's translations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextOwner {
    Channel,
    Category,
    Product,
    SelectionGroup,
    SelectionOption,
}

impl TextOwner {
    const fn as_str(self) -> &'static str {
        match self {
            TextOwner::Channel => "CHANNEL",
            TextOwner::Category => "CATEGORY",
            TextOwner::Product => "PRODUCT",
            TextOwner::SelectionGroup => "SELECTION_GROUP",
            TextOwner::SelectionOption => "SELECTION_OPTION",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TextField {
    Name,
    DisplayName,
    Description,
}

impl TextField {
    const fn as_str(self) -> &'static str {
        match self {
            TextField::Name => "NAME",
            TextField::DisplayName => "DISPLAY_NAME",
            TextField::Description => "DESCRIPTION",
        }
    }
}

/// Translations of one owner kind, keyed by `(owner_ref, field)`.
type Translations = BTreeMap<(String, TextField), BTreeMap<Locale, String>>;

/// Rebuild one localized string from its default and the translations loaded
/// for its owner kind. Takes the translations out of the map: each is consumed
/// by exactly one field of one row.
fn localized(
    translations: &mut Translations,
    owner_ref: &str,
    field: TextField,
    default: String,
) -> Result<LocalizedText, AppError> {
    let found = translations
        .remove(&(owner_ref.to_owned(), field))
        .unwrap_or_default();
    LocalizedText::with_translations(default, found).map_err(AppError::Catalog)
}

/// The three columns an optional image reference occupies. Nothing stores
/// bytes: PI.Edge holds the content hash and the Asset Runtime holds the file.
struct AssetColumns {
    content_hash: Option<String>,
    media_type: Option<String>,
    byte_size: Option<i64>,
}

impl AssetColumns {
    fn from(asset: Option<&AssetRef>) -> Self {
        match asset {
            Some(asset) => Self {
                content_hash: Some(asset.content_hash.clone()),
                media_type: Some(asset.media_type.clone()),
                byte_size: Some(asset.byte_size),
            },
            None => Self {
                content_hash: None,
                media_type: None,
                byte_size: None,
            },
        }
    }

    fn from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            content_hash: row.get(offset)?,
            media_type: row.get(offset + 1)?,
            byte_size: row.get(offset + 2)?,
        })
    }

    fn into_domain(self) -> Result<Option<AssetRef>, AppError> {
        match (self.content_hash, self.media_type, self.byte_size) {
            (Some(content_hash), Some(media_type), Some(byte_size)) => {
                AssetRef::import(content_hash, media_type, byte_size)
                    .map(Some)
                    .map_err(AppError::Catalog)
            }
            // The table's CHECKs keep the three columns in step, so a partial
            // asset can only mean the row was written by something else.
            (None, None, None) => Ok(None),
            _ => Err(AppError::Catalog(CatalogError::InvalidRecord {
                field: "image",
                reason: "an asset reference is incomplete".to_owned(),
            })),
        }
    }
}

impl SqliteUnitOfWork<'_, '_> {
    /// One product's merchandising list. Declared order is persisted, because
    /// what a screen offers first is content.
    fn insert_catalog_recommendations(
        &mut self,
        snapshot_id: &str,
        row: &ProductPresentation,
    ) -> Result<(), AppError> {
        row.validate_recommendations().map_err(AppError::Catalog)?;
        for (position, recommended) in row.recommended_product_refs.iter().enumerate() {
            self.tx
                .execute(
                    "INSERT INTO catalog_product_recommendations
                         (snapshot_id, product_ref, recommended_product_ref, display_order)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        snapshot_id,
                        row.product_ref.as_str(),
                        recommended.as_str(),
                        i64::try_from(position).unwrap_or(i64::MAX),
                    ],
                )
                .map_err(|error| {
                    if is_foreign_key_violation(&error) {
                        // The suggestion names a product this snapshot does
                        // not sell. Refuse the snapshot rather than activate a
                        // menu that offers something nobody can add.
                        AppError::Catalog(CatalogError::PresentationWithoutProduct {
                            product_ref: recommended.to_string(),
                        })
                    } else if is_constraint_violation(&error) {
                        AppError::Catalog(CatalogError::DuplicatePresentation {
                            product_ref: recommended.to_string(),
                        })
                    } else {
                        sqlite_err(error)
                    }
                })?;
        }
        Ok(())
    }

    /// Every snapshot's recommendation lists at once, keyed by the product
    /// being shown. One query, so rendering a menu is not N+1.
    fn load_recommendations(
        &mut self,
        snapshot_id: &str,
    ) -> Result<BTreeMap<String, Vec<ProductRef>>, AppError> {
        let mut statement = self
            .tx
            .prepare(
                "SELECT product_ref, recommended_product_ref
                 FROM catalog_product_recommendations
                 WHERE snapshot_id = ?1
                 ORDER BY product_ref ASC, display_order ASC",
            )
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![snapshot_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        let mut grouped: BTreeMap<String, Vec<ProductRef>> = BTreeMap::new();
        for (product_ref, recommended) in rows {
            grouped
                .entry(product_ref)
                .or_default()
                .push(parse_id(&recommended)?);
        }
        Ok(grouped)
    }

    fn insert_translations(
        &mut self,
        snapshot_id: &str,
        owner: TextOwner,
        owner_ref: &str,
        field: TextField,
        text: &LocalizedText,
    ) -> Result<(), AppError> {
        for (locale, translation) in &text.translations {
            self.tx
                .execute(
                    "INSERT INTO catalog_text_translations
                         (snapshot_id, owner_kind, owner_ref, field, locale, text)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        snapshot_id,
                        owner.as_str(),
                        owner_ref,
                        field.as_str(),
                        locale.as_str(),
                        translation,
                    ],
                )
                .map_err(sqlite_err)?;
        }
        Ok(())
    }

    /// Every translation of one owner kind, in one query, so listing rows is
    /// never one query per row.
    fn load_translations(
        &mut self,
        snapshot_id: &str,
        owner: TextOwner,
    ) -> Result<Translations, AppError> {
        let mut statement = self
            .tx
            .prepare(
                "SELECT owner_ref, field, locale, text FROM catalog_text_translations
                 WHERE snapshot_id = ?1 AND owner_kind = ?2
                 ORDER BY owner_ref ASC, field ASC, locale ASC",
            )
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![snapshot_id, owner.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;

        let mut translations = Translations::new();
        for (owner_ref, field, locale, text) in rows {
            let field = match field.as_str() {
                "NAME" => TextField::Name,
                "DISPLAY_NAME" => TextField::DisplayName,
                "DESCRIPTION" => TextField::Description,
                other => {
                    return Err(AppError::Catalog(CatalogError::InvalidRecord {
                        field: "translation.field",
                        reason: format!("`{other}` is not a localizable field"),
                    }))
                }
            };
            translations
                .entry((owner_ref, field))
                .or_default()
                .insert(Locale::new(locale).map_err(AppError::Catalog)?, text);
        }
        Ok(translations)
    }

    fn count(&self, table: &'static str, snapshot_id: &str) -> Result<i64, AppError> {
        self.tx
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE snapshot_id = ?1"),
                params![snapshot_id],
                |row| row.get(0),
            )
            .map_err(sqlite_err)
    }
}

fn is_constraint_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

fn is_foreign_key_violation(error: &rusqlite::Error) -> bool {
    match error {
        rusqlite::Error::SqliteFailure(failure, message) => {
            failure.code == rusqlite::ErrorCode::ConstraintViolation
                && message
                    .as_deref()
                    .is_some_and(|text| text.contains("FOREIGN KEY"))
        }
        _ => false,
    }
}

struct RawSnapshot {
    snapshot_id: String,
    tenant_id: String,
    store_id: String,
    version: i64,
    status: String,
    declared_content_hash: String,
    content_hash: Option<String>,
    generated_at: String,
    effective_at: Option<String>,
    expected_tax_profiles: i64,
    expected_price_books: i64,
    expected_products: i64,
    expected_price_entries: i64,
    expected_barcodes: i64,
    expected_promotions: i64,
    imported_at: String,
    committed_at: Option<String>,
    activated_at: Option<String>,
    superseded_at: Option<String>,
    expected_channels: i64,
    expected_categories: i64,
    expected_presentation: i64,
    expected_selection_groups: i64,
    expected_selection_options: i64,
    expected_availability: i64,
}

impl RawSnapshot {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            snapshot_id: row.get(0)?,
            tenant_id: row.get(1)?,
            store_id: row.get(2)?,
            version: row.get(3)?,
            status: row.get(4)?,
            declared_content_hash: row.get(5)?,
            content_hash: row.get(6)?,
            generated_at: row.get(7)?,
            effective_at: row.get(8)?,
            expected_tax_profiles: row.get(9)?,
            expected_price_books: row.get(10)?,
            expected_products: row.get(11)?,
            expected_price_entries: row.get(12)?,
            expected_barcodes: row.get(13)?,
            expected_promotions: row.get(14)?,
            imported_at: row.get(15)?,
            committed_at: row.get(16)?,
            activated_at: row.get(17)?,
            superseded_at: row.get(18)?,
            expected_channels: row.get(19)?,
            expected_categories: row.get(20)?,
            expected_presentation: row.get(21)?,
            expected_selection_groups: row.get(22)?,
            expected_selection_options: row.get(23)?,
            expected_availability: row.get(24)?,
        })
    }

    fn into_domain(self) -> Result<CatalogSnapshot, AppError> {
        Ok(CatalogSnapshot::rehydrate(CatalogSnapshotState {
            snapshot_id: parse_id(&self.snapshot_id)?,
            tenant_id: parse_id(&self.tenant_id)?,
            store_id: parse_id(&self.store_id)?,
            version: self.version,
            status: CatalogSnapshotStatus::parse(&self.status).map_err(AppError::Catalog)?,
            declared_content_hash: self.declared_content_hash,
            content_hash: self.content_hash,
            generated_at: parse_ts(&self.generated_at)?,
            effective_at: parse_opt_ts(self.effective_at)?,
            expected: SnapshotCounts {
                tax_profiles: self.expected_tax_profiles,
                price_books: self.expected_price_books,
                products: self.expected_products,
                price_entries: self.expected_price_entries,
                barcodes: self.expected_barcodes,
                promotions: self.expected_promotions,
                channels: self.expected_channels,
                categories: self.expected_categories,
                presentation: self.expected_presentation,
                selection_groups: self.expected_selection_groups,
                selection_options: self.expected_selection_options,
                availability: self.expected_availability,
            },
            imported_at: parse_ts(&self.imported_at)?,
            committed_at: parse_opt_ts(self.committed_at)?,
            activated_at: parse_opt_ts(self.activated_at)?,
            superseded_at: parse_opt_ts(self.superseded_at)?,
        }))
    }
}

struct RawTaxProfile {
    code: String,
    name: String,
    rate_bps: i64,
}

impl RawTaxProfile {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            code: row.get(0)?,
            name: row.get(1)?,
            rate_bps: row.get(2)?,
        })
    }

    fn into_domain(self) -> Result<TaxProfile, AppError> {
        let bps = u32::try_from(self.rate_bps).map_err(|_| {
            AppError::Catalog(CatalogError::InvalidRecord {
                field: "rate_bps",
                reason: "out of range".to_owned(),
            })
        })?;
        TaxProfile::new(
            TaxProfileCode::new(self.code).map_err(AppError::Catalog)?,
            self.name,
            TaxRate::from_bps(bps).map_err(|e| AppError::Catalog(CatalogError::Money(e)))?,
        )
        .map_err(AppError::Catalog)
    }
}

struct RawPriceBook {
    price_book_id: String,
    code: String,
    name: String,
    currency: String,
    scope: String,
    scope_store_id: Option<String>,
    priority: i64,
    active: i64,
}

impl RawPriceBook {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            price_book_id: row.get(0)?,
            code: row.get(1)?,
            name: row.get(2)?,
            currency: row.get(3)?,
            scope: row.get(4)?,
            scope_store_id: row.get(5)?,
            priority: row.get(6)?,
            active: row.get(7)?,
        })
    }

    fn into_domain(self) -> Result<PriceBook, AppError> {
        let scope = match (self.scope.as_str(), self.scope_store_id) {
            ("TENANT", _) => PriceBookScope::Tenant,
            ("STORE", Some(store_id)) => PriceBookScope::Store {
                store_id: parse_id(&store_id)?,
            },
            (other, _) => {
                return Err(AppError::Catalog(CatalogError::InvalidRecord {
                    field: "price_book_scope",
                    reason: format!("unknown or incomplete scope `{other}`"),
                }))
            }
        };
        PriceBook::import(NewPriceBook {
            price_book_id: parse_id(&self.price_book_id)?,
            code: self.code,
            name: self.name,
            currency: parse_currency(&self.currency)?,
            scope,
            priority: i32::try_from(self.priority).map_err(|_| {
                AppError::Catalog(CatalogError::InvalidRecord {
                    field: "price_book_priority",
                    reason: "out of range".to_owned(),
                })
            })?,
            active: self.active != 0,
        })
        .map_err(AppError::Catalog)
    }
}

struct RawPriceEntry {
    price_book_id: String,
    product_ref: String,
    amount_minor: i64,
    currency: String,
    price_mode: String,
    valid_from: Option<String>,
    valid_until: Option<String>,
}

impl RawPriceEntry {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Self::from_offset(row, 0)
    }

    fn from_offset(row: &Row<'_>, offset: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            price_book_id: row.get(offset)?,
            product_ref: row.get(offset + 1)?,
            amount_minor: row.get(offset + 2)?,
            currency: row.get(offset + 3)?,
            price_mode: row.get(offset + 4)?,
            valid_from: row.get(offset + 5)?,
            valid_until: row.get(offset + 6)?,
        })
    }

    fn into_domain(self) -> Result<PriceEntry, AppError> {
        let currency = parse_currency(&self.currency)?;
        PriceEntry::import(NewPriceEntry {
            price_book_id: parse_id(&self.price_book_id)?,
            product_ref: parse_id(&self.product_ref)?,
            amount: money(self.amount_minor, currency),
            price_mode: PriceMode::parse(&self.price_mode)
                .map_err(|e| AppError::Catalog(CatalogError::Money(e)))?,
            valid_from: parse_opt_ts(self.valid_from)?,
            valid_until: parse_opt_ts(self.valid_until)?,
        })
        .map_err(AppError::Catalog)
    }
}

struct RawProduct {
    product_ref: String,
    external_ref: Option<String>,
    sku: Option<String>,
    name: String,
    active: i64,
    sellable: i64,
    tax_profile_code: String,
}

impl RawProduct {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            product_ref: row.get(0)?,
            external_ref: row.get(1)?,
            sku: row.get(2)?,
            name: row.get(3)?,
            active: row.get(4)?,
            sellable: row.get(5)?,
            tax_profile_code: row.get(6)?,
        })
    }

    fn into_domain(self) -> Result<CatalogProduct, AppError> {
        Ok(CatalogProduct {
            product_ref: parse_id(&self.product_ref)?,
            external_ref: self.external_ref,
            sku: self.sku,
            name: self.name,
            active: self.active != 0,
            sellable: self.sellable != 0,
            tax_profile: TaxProfileCode::new(self.tax_profile_code).map_err(AppError::Catalog)?,
        })
    }
}

/// The typed rule columns of one promotion. Each rule kind fills its own slots
/// and leaves the rest NULL; the table's CHECK enforces exactly that, so a
/// malformed rule cannot be stored in the first place.
struct RuleColumns {
    product_ref: Option<String>,
    percentage_bps: Option<i64>,
    amount_minor: Option<i64>,
    minimum_minor: Option<i64>,
    currency: Option<&'static str>,
    buy_thousandths: Option<i64>,
    get_thousandths: Option<i64>,
}

impl RuleColumns {
    fn from(rule: &PromotionRule) -> Self {
        let product_ref = rule
            .applicability()
            .and_then(|applies| applies.product_ref())
            .map(|product| product.to_string());
        let empty = Self {
            product_ref,
            percentage_bps: None,
            amount_minor: None,
            minimum_minor: None,
            currency: None,
            buy_thousandths: None,
            get_thousandths: None,
        };
        match rule {
            PromotionRule::PercentageOff { percentage, .. } => Self {
                percentage_bps: Some(i64::from(percentage.bps())),
                ..empty
            },
            PromotionRule::FixedAmountOff { amount, .. } => Self {
                amount_minor: Some(amount.minor()),
                currency: Some(amount.currency().code()),
                ..empty
            },
            PromotionRule::BuyNGetM { buy, get, .. } => Self {
                buy_thousandths: Some(buy.thousandths()),
                get_thousandths: Some(get.thousandths()),
                ..empty
            },
            PromotionRule::BasketThreshold { minimum, amount } => Self {
                amount_minor: Some(amount.minor()),
                minimum_minor: Some(minimum.minor()),
                currency: Some(amount.currency().code()),
                ..empty
            },
        }
    }
}

struct RawPromotion {
    promotion_id: String,
    code: String,
    name: String,
    active: i64,
    priority: i64,
    stackability: String,
    scope: String,
    scope_store_id: Option<String>,
    valid_from: Option<String>,
    valid_until: Option<String>,
    rule_kind: String,
    applies_to_product_ref: Option<String>,
    percentage_bps: Option<i64>,
    amount_minor: Option<i64>,
    minimum_minor: Option<i64>,
    currency: Option<String>,
    buy_thousandths: Option<i64>,
    get_thousandths: Option<i64>,
}

impl RawPromotion {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            promotion_id: row.get(0)?,
            code: row.get(1)?,
            name: row.get(2)?,
            active: row.get(3)?,
            priority: row.get(4)?,
            stackability: row.get(5)?,
            scope: row.get(6)?,
            scope_store_id: row.get(7)?,
            valid_from: row.get(8)?,
            valid_until: row.get(9)?,
            rule_kind: row.get(10)?,
            applies_to_product_ref: row.get(11)?,
            percentage_bps: row.get(12)?,
            amount_minor: row.get(13)?,
            minimum_minor: row.get(14)?,
            currency: row.get(15)?,
            buy_thousandths: row.get(16)?,
            get_thousandths: row.get(17)?,
        })
    }

    fn into_domain(self) -> Result<Promotion, AppError> {
        let scope = match (self.scope.as_str(), self.scope_store_id.as_deref()) {
            ("TENANT", _) => PriceBookScope::Tenant,
            ("STORE", Some(store_id)) => PriceBookScope::Store {
                store_id: parse_id(store_id)?,
            },
            (other, _) => {
                return Err(AppError::Catalog(CatalogError::InvalidRecord {
                    field: "promotion_scope",
                    reason: format!("unknown or incomplete scope `{other}`"),
                }))
            }
        };
        let applies_to = match self.applies_to_product_ref.as_deref() {
            Some(product_ref) => ProductApplicability::Product {
                product_ref: parse_id(product_ref)?,
            },
            None => ProductApplicability::AnyProduct,
        };
        let rule = match self.rule_kind.as_str() {
            "PERCENTAGE_OFF" => PromotionRule::PercentageOff {
                applies_to,
                percentage: Percentage::from_bps(
                    u32::try_from(required(self.percentage_bps, "percentage_bps")?).map_err(
                        |_| {
                            AppError::Catalog(CatalogError::InvalidRecord {
                                field: "percentage_bps",
                                reason: "out of range".to_owned(),
                            })
                        },
                    )?,
                )
                .map_err(|error| AppError::Catalog(CatalogError::from(error)))?,
            },
            "FIXED_AMOUNT_OFF" => PromotionRule::FixedAmountOff {
                applies_to,
                amount: self.money("amount_minor", self.amount_minor)?,
            },
            "BUY_N_GET_M" => PromotionRule::BuyNGetM {
                applies_to,
                buy: quantity(required(self.buy_thousandths, "buy_thousandths")?)?,
                get: quantity(required(self.get_thousandths, "get_thousandths")?)?,
            },
            "BASKET_THRESHOLD" => PromotionRule::BasketThreshold {
                minimum: self.money("minimum_minor", self.minimum_minor)?,
                amount: self.money("amount_minor", self.amount_minor)?,
            },
            other => {
                return Err(AppError::Catalog(CatalogError::InvalidRecord {
                    field: "rule_kind",
                    reason: format!("unknown rule `{other}`"),
                }))
            }
        };
        Promotion::import(NewPromotion {
            promotion_id: parse_id(&self.promotion_id)?,
            code: self.code,
            name: self.name,
            active: self.active != 0,
            priority: i32::try_from(self.priority).map_err(|_| {
                AppError::Catalog(CatalogError::InvalidRecord {
                    field: "promotion_priority",
                    reason: "out of range".to_owned(),
                })
            })?,
            stackability: Stackability::parse(&self.stackability).map_err(AppError::Catalog)?,
            scope,
            valid_from: parse_opt_ts(self.valid_from)?,
            valid_until: parse_opt_ts(self.valid_until)?,
            rule,
        })
        .map_err(AppError::Catalog)
    }

    fn money(&self, field: &'static str, minor: Option<i64>) -> Result<Money, AppError> {
        let code =
            self.currency
                .as_deref()
                .ok_or(AppError::Catalog(CatalogError::InvalidRecord {
                    field: "currency",
                    reason: "missing for a money rule".to_owned(),
                }))?;
        Ok(money(required(minor, field)?, parse_currency(code)?))
    }
}

fn required(value: Option<i64>, field: &'static str) -> Result<i64, AppError> {
    value.ok_or(AppError::Catalog(CatalogError::InvalidRecord {
        field,
        reason: "missing for this rule kind".to_owned(),
    }))
}

fn quantity(thousandths: i64) -> Result<Quantity, AppError> {
    Quantity::from_thousandths(thousandths)
        .map_err(|error| AppError::Catalog(CatalogError::from(error)))
}
