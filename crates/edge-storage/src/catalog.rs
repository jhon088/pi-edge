//! SQLite adapter for the catalog context.
//!
//! Lookups on the sale path are index hits by construction: a product by
//! `(snapshot_id, product_ref)` and a barcode by `(snapshot_id, barcode)` are
//! primary keys, and `sku` has its own index. Nothing here scans.
//!
//! The content hash is computed by streaming the stored rows in canonical
//! order through the catalog's hasher, so it describes what was persisted
//! rather than what was received.

use edge_application::ports::{CatalogUnitOfWork, StoredChunk};
use edge_application::AppError;
use edge_catalog::content::ContentHasher;
use edge_catalog::pricing::{NewPriceBook, NewPriceEntry, PriceBookScope};
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
     imported_at, committed_at, activated_at, superseded_at";

const PRODUCT_COLUMNS: &str = "product_ref, external_ref, sku, name, active, sellable, \
     tax_profile_code";

const PRICE_BOOK_COLUMNS: &str =
    "price_book_id, code, name, currency, scope, scope_store_id, priority, active";

const PRICE_ENTRY_COLUMNS: &str =
    "price_book_id, product_ref, amount_minor, currency, price_mode, valid_from, valid_until";

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
                      expected_barcodes, imported_at, committed_at, activated_at, superseded_at,
                      created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?19)",
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
                    state.imported_at.to_rfc3339(),
                    state.committed_at.map(|ts| ts.to_rfc3339()),
                    state.activated_at.map(|ts| ts.to_rfc3339()),
                    state.superseded_at.map(|ts| ts.to_rfc3339()),
                    now.to_rfc3339(),
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
                "SELECT chunk_index, chunk_hash, product_count, price_entry_count, barcode_count
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
                      barcode_count, received_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    snapshot_id.to_string(),
                    chunk.chunk_index,
                    chunk.chunk_hash,
                    chunk.product_count,
                    chunk.price_entry_count,
                    chunk.barcode_count,
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

        Ok(hasher.finish())
    }
}

impl SqliteUnitOfWork<'_, '_> {
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
    imported_at: String,
    committed_at: Option<String>,
    activated_at: Option<String>,
    superseded_at: Option<String>,
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
            imported_at: row.get(14)?,
            committed_at: row.get(15)?,
            activated_at: row.get(16)?,
            superseded_at: row.get(17)?,
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
