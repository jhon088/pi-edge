//! Catalog use cases: import, activation and lookup.
//!
//! The import is a chunked, resumable protocol rather than one big payload,
//! because a catalog is eventually thousands of products and the runtime must
//! never hold one in memory (ADR 012):
//!
//! ```text
//! BeginCatalogImport   -> IMPORTING, tax profiles stored, progress returned
//! PutCatalogChunk  xN  -> validate -> persist in its own transaction -> ack
//! CommitCatalogImport  -> verify counts, contiguity and content hash -> PENDING
//! ActivateCatalogSnapshot -> previous ACTIVE superseded and this one activated,
//!                            in one transaction
//! ```
//!
//! Every step is durable on its own. A failed commit leaves the import in
//! `IMPORTING`, which is exactly the state a client resumes from.

use edge_catalog::promotions::PromotionRule;
use edge_catalog::{
    resolve_price, validate_availability_targets, validate_channels, validate_selection_structure,
    CatalogError, CatalogSnapshot, CatalogSnapshotStatus, NewCatalogSnapshot, PricingContext,
    ProductSelector, SnapshotCounts,
};
use edge_domain::prelude::*;
use serde_json::json;

use crate::commands::{
    AbortCatalogImportCommand, ActivateCatalogSnapshotCommand, BeginCatalogImportCommand,
    CommitCatalogImportCommand, PutCatalogChunkCommand,
};
use crate::context::{
    idempotent, journal_event, require_device, require_open_business_day, EventContext,
};
use crate::error::AppError;
use crate::ports::{Clock, EdgeStore, IdFactory, StoredChunk, UnitOfWork};
use crate::queries::{CatalogImportStatus, CatalogSnapshotView, CatalogStatus, ProductLookup};
use crate::EdgeApplication;

impl<S, C, I, P> EdgeApplication<S, C, I, P>
where
    S: EdgeStore,
    C: Clock,
    I: IdFactory,
    P: edge_payments::PaymentProviderPort,
{
    /// Open an import, or return the one already open for this content.
    pub fn begin_catalog_import(
        &self,
        cmd: BeginCatalogImportCommand,
    ) -> Result<CatalogImportStatus, AppError> {
        let now = self.clock.now();
        let snapshot_id = CatalogSnapshotId::from_uuid(self.ids.new_uuid());

        let status = self.store().transact(move |uow| {
            idempotent(uow, cmd.request_id, "BeginCatalogImport", now, |uow| {
                let device = require_device(uow)?;

                // Resuming is the same call: the open import for this exact
                // content is returned with its progress.
                if let Some(existing) =
                    uow.find_catalog_snapshot_by_version(&device.store_id, cmd.version)?
                {
                    if existing.state().declared_content_hash != cmd.declared_content_hash {
                        return Err(CatalogError::VersionConflict {
                            version: cmd.version,
                        }
                        .into());
                    }
                    let progress = uow.catalog_import_progress(&existing)?;
                    return Ok(CatalogImportStatus {
                        snapshot: existing,
                        progress,
                    });
                }

                if cmd.currency != device.currency {
                    return Err(CatalogError::CurrencyMismatch {
                        expected: device.currency.code(),
                        actual: cmd.currency.code(),
                    }
                    .into());
                }
                if cmd.tax_profiles.is_empty() {
                    return Err(CatalogError::InvalidRecord {
                        field: "tax_profiles",
                        reason: "a snapshot must declare at least one tax profile".to_owned(),
                    }
                    .into());
                }
                // Without a price book nothing can be sold, so a snapshot that
                // declares none is rejected at the door rather than at the till.
                if cmd.price_books.is_empty() {
                    return Err(CatalogError::InvalidRecord {
                        field: "price_books",
                        reason: "a snapshot must declare at least one price book".to_owned(),
                    }
                    .into());
                }
                for book in &cmd.price_books {
                    if book.currency != device.currency {
                        return Err(CatalogError::CurrencyMismatch {
                            expected: device.currency.code(),
                            actual: book.currency.code(),
                        }
                        .into());
                    }
                }
                if i64::try_from(cmd.price_books.len()).unwrap_or(i64::MAX)
                    != cmd.expected.price_books
                {
                    return Err(CatalogError::CountMismatch {
                        what: "price books",
                        expected: cmd.expected.price_books,
                        stored: i64::try_from(cmd.price_books.len()).unwrap_or(i64::MAX),
                    }
                    .into());
                }
                if cmd.expected.products <= 0 {
                    return Err(CatalogError::InvalidRecord {
                        field: "expected_products",
                        reason: "a snapshot must declare at least one product".to_owned(),
                    }
                    .into());
                }
                if i64::try_from(cmd.tax_profiles.len()).unwrap_or(i64::MAX)
                    != cmd.expected.tax_profiles
                {
                    return Err(CatalogError::CountMismatch {
                        what: "tax profiles",
                        expected: cmd.expected.tax_profiles,
                        stored: i64::try_from(cmd.tax_profiles.len()).unwrap_or(i64::MAX),
                    }
                    .into());
                }
                // The header's own extensions are declared and counted here;
                // the bulk ones are counted per chunk and verified at commit.
                if i64::try_from(cmd.channels.len()).unwrap_or(i64::MAX) != cmd.expected.channels {
                    return Err(CatalogError::CountMismatch {
                        what: "channels",
                        expected: cmd.expected.channels,
                        stored: i64::try_from(cmd.channels.len()).unwrap_or(i64::MAX),
                    }
                    .into());
                }
                if i64::try_from(cmd.categories.len()).unwrap_or(i64::MAX)
                    != cmd.expected.categories
                {
                    return Err(CatalogError::CountMismatch {
                        what: "categories",
                        expected: cmd.expected.categories,
                        stored: i64::try_from(cmd.categories.len()).unwrap_or(i64::MAX),
                    }
                    .into());
                }
                // Duplicate channels are refused before anything is written,
                // so the header is never half stored.
                validate_channels(&cmd.channels, &[])?;

                let snapshot = CatalogSnapshot::begin_import(NewCatalogSnapshot {
                    snapshot_id,
                    tenant_id: device.tenant_id.clone(),
                    store_id: device.store_id.clone(),
                    version: cmd.version,
                    declared_content_hash: cmd.declared_content_hash.clone(),
                    generated_at: cmd.generated_at,
                    effective_at: cmd.effective_at,
                    expected: cmd.expected,
                    imported_at: now,
                });
                uow.insert_catalog_snapshot(&snapshot, now)?;
                uow.insert_catalog_tax_profiles(snapshot.id(), &cmd.tax_profiles)?;
                uow.insert_catalog_price_books(snapshot.id(), &cmd.price_books)?;
                uow.insert_catalog_channels(snapshot.id(), &cmd.channels)?;
                uow.insert_catalog_categories(snapshot.id(), &cmd.categories)?;
                let progress = uow.catalog_import_progress(&snapshot)?;
                Ok(CatalogImportStatus { snapshot, progress })
            })
        })?;
        tracing::info!(
            snapshot_id = %status.snapshot.id(),
            version = status.snapshot.version(),
            status = status.snapshot.status().as_str(),
            received_chunks = status.progress.received_chunks.len(),
            "catalog import open"
        );
        Ok(status)
    }

    /// Store one chunk. Durable, validated and idempotent on its content.
    pub fn put_catalog_chunk(
        &self,
        cmd: PutCatalogChunkCommand,
    ) -> Result<CatalogImportStatus, AppError> {
        let now = self.clock.now();
        self.store().transact(move |uow| {
            let snapshot = load_snapshot(uow, cmd.snapshot_id)?;

            // Replay handling before anything else: identical content is a
            // no-op, different content for the same index is a hard conflict.
            //
            // Checked *before* the import is required to be open, because a
            // replay is a replay whatever the snapshot has become since. A
            // delivery channel that retries — which milestone 11's at-least-
            // once sync is — re-sends chunks of an import that may already be
            // committed or in force, and the honest answer to "I already have
            // exactly this" is yes, not `IMPORT_NOT_OPEN`.
            if let Some(stored) = uow.find_catalog_chunk(cmd.snapshot_id, cmd.chunk_index)? {
                if stored.chunk_hash == cmd.chunk_hash {
                    tracing::debug!(
                        snapshot_id = %cmd.snapshot_id,
                        chunk_index = cmd.chunk_index,
                        "chunk already stored; acknowledging replay"
                    );
                    let progress = uow.catalog_import_progress(&snapshot)?;
                    return Ok(CatalogImportStatus { snapshot, progress });
                }
                return Err(CatalogError::ChunkConflict {
                    index: cmd.chunk_index,
                }
                .into());
            }

            // New content, so the import has to still be open for it.
            snapshot.require_importing()?;

            // Validate the whole chunk before writing any of it: a malformed
            // record must not leave half a chunk behind.
            let profiles = uow.list_catalog_tax_profiles(cmd.snapshot_id)?;
            for product in &cmd.products {
                if !profiles
                    .iter()
                    .any(|profile| profile.code == product.tax_profile)
                {
                    return Err(CatalogError::UnknownTaxProfile {
                        product_ref: product.product_ref.to_string(),
                        code: product.tax_profile.to_string(),
                    }
                    .into());
                }
            }
            let books = uow.list_catalog_price_books(cmd.snapshot_id)?;
            for entry in &cmd.price_entries {
                if !books
                    .iter()
                    .any(|book| book.price_book_id == entry.price_book_id)
                {
                    return Err(CatalogError::UnknownPriceBook {
                        price_book_id: entry.price_book_id.to_string(),
                    }
                    .into());
                }
            }

            // A promotion's money must be in the snapshot's currency, and a
            // promotion that names a product must travel with that product.
            let currency = books
                .first()
                .map(|book| book.currency)
                .unwrap_or(Currency::Mxn);
            for promotion in &cmd.promotions {
                for amount in promotion_amounts(&promotion.rule) {
                    if amount.currency() != currency {
                        return Err(CatalogError::CurrencyMismatch {
                            expected: currency.code(),
                            actual: amount.currency().code(),
                        }
                        .into());
                    }
                }
                if let Some(product_ref) = promotion
                    .rule
                    .applicability()
                    .and_then(|applies| applies.product_ref())
                {
                    if !cmd
                        .products
                        .iter()
                        .any(|product| &product.product_ref == product_ref)
                    {
                        return Err(CatalogError::InvalidRecord {
                            field: "promotion_applies_to",
                            reason: format!(
                                "promotion `{}` names product `{product_ref}`, which is not in \
                                 this chunk",
                                promotion.promotion_id
                            ),
                        }
                        .into());
                    }
                }
            }

            uow.insert_catalog_products(cmd.snapshot_id, &cmd.products)?;
            uow.insert_catalog_price_entries(cmd.snapshot_id, &cmd.price_entries)?;
            uow.insert_catalog_barcodes(cmd.snapshot_id, &cmd.barcodes)?;
            uow.insert_catalog_promotions(cmd.snapshot_id, &cmd.promotions)?;
            // Order matters: presentation and selection content reference the
            // products this chunk just stored, and an option references its
            // group. A group owned by an option is the one reference that may
            // point outside the chunk, which is why the owner chain is
            // validated at commit instead.
            uow.insert_catalog_presentation(cmd.snapshot_id, &cmd.presentation)?;
            uow.insert_catalog_selection_groups(cmd.snapshot_id, &cmd.selection_groups)?;
            uow.insert_catalog_selection_options(cmd.snapshot_id, &cmd.selection_options)?;
            uow.insert_catalog_availability(cmd.snapshot_id, &cmd.availability)?;
            uow.record_catalog_chunk(
                cmd.snapshot_id,
                &StoredChunk {
                    chunk_index: cmd.chunk_index,
                    chunk_hash: cmd.chunk_hash.clone(),
                    product_count: i64::try_from(cmd.products.len()).unwrap_or(i64::MAX),
                    price_entry_count: i64::try_from(cmd.price_entries.len()).unwrap_or(i64::MAX),
                    barcode_count: i64::try_from(cmd.barcodes.len()).unwrap_or(i64::MAX),
                    promotion_count: i64::try_from(cmd.promotions.len()).unwrap_or(i64::MAX),
                    presentation_count: i64::try_from(cmd.presentation.len()).unwrap_or(i64::MAX),
                    selection_group_count: i64::try_from(cmd.selection_groups.len())
                        .unwrap_or(i64::MAX),
                    selection_option_count: i64::try_from(cmd.selection_options.len())
                        .unwrap_or(i64::MAX),
                    availability_count: i64::try_from(cmd.availability.len()).unwrap_or(i64::MAX),
                },
                now,
            )?;
            let progress = uow.catalog_import_progress(&snapshot)?;
            Ok(CatalogImportStatus { snapshot, progress })
        })
    }

    /// Verify and close the import. Only a fully verified snapshot becomes
    /// `PENDING`; anything else leaves it `IMPORTING` and resumable.
    pub fn commit_catalog_import(
        &self,
        cmd: CommitCatalogImportCommand,
    ) -> Result<CatalogSnapshotView, AppError> {
        let now = self.clock.now();
        let event_id = EventId::from_uuid(self.ids.new_uuid());

        let view = self.store().transact(move |uow| {
            idempotent(uow, cmd.request_id, "CommitCatalogImport", now, |uow| {
                let device = require_device(uow)?;
                let mut snapshot = load_snapshot(uow, cmd.snapshot_id)?;

                // A committed snapshot is already closed; return it as is.
                if snapshot.status() != CatalogSnapshotStatus::Importing {
                    let stored = uow.catalog_stored_counts(cmd.snapshot_id)?;
                    return Ok(CatalogSnapshotView { snapshot, stored });
                }

                let progress = uow.catalog_import_progress(&snapshot)?;
                progress.require_contiguous()?;
                let received = i64::try_from(progress.received_chunks.len()).unwrap_or(i64::MAX);
                if received != cmd.chunk_count {
                    return Err(CatalogError::MissingChunk { index: received }.into());
                }
                snapshot.verify_counts(progress.stored)?;
                verify_catalog_structure(uow, cmd.snapshot_id, progress.stored)?;

                // The hash the core trusts is the one it computes from its own
                // rows, in canonical order.
                let computed = uow.compute_catalog_content_hash(cmd.snapshot_id)?;
                snapshot.verify_content_hash(&computed)?;

                snapshot.mark_pending(computed.clone(), now)?;
                uow.update_catalog_snapshot(&snapshot, now)?;

                let ctx = EventContext {
                    device: &device,
                    register_id: None,
                    business_day_id: None,
                    session_id: None,
                    transaction_id: None,
                    operator_id: None,
                };
                uow.append_journal(&journal_event(
                    event_id,
                    JournalEventType::CatalogSnapshotImported,
                    snapshot.id().to_string(),
                    &ctx,
                    now,
                    json!({
                        "version": snapshot.version(),
                        "content_hash": computed,
                        "products": progress.stored.products,
                        "barcodes": progress.stored.barcodes,
                        "tax_profiles": progress.stored.tax_profiles,
                        "chunks": cmd.chunk_count,
                    }),
                ))?;
                Ok(CatalogSnapshotView {
                    snapshot,
                    stored: progress.stored,
                })
            })
        })?;
        tracing::info!(
            snapshot_id = %view.snapshot.id(),
            version = view.snapshot.version(),
            products = view.stored.products,
            "catalog snapshot committed"
        );
        Ok(view)
    }

    /// Discard an open import and everything staged for it.
    pub fn abort_catalog_import(&self, cmd: AbortCatalogImportCommand) -> Result<(), AppError> {
        let now = self.clock.now();
        self.store().transact(move |uow| {
            idempotent(uow, cmd.request_id, "AbortCatalogImport", now, |uow| {
                let snapshot = load_snapshot(uow, cmd.snapshot_id)?;
                snapshot.require_importing()?;
                uow.delete_catalog_snapshot(cmd.snapshot_id)?;
                Ok(())
            })
        })?;
        tracing::info!(snapshot_id = %cmd.snapshot_id, "catalog import aborted");
        Ok(())
    }

    /// Put a committed snapshot in force.
    ///
    /// Superseding the incumbent and activating the successor happen in one
    /// transaction, so no reader can observe zero or two active snapshots.
    pub fn activate_catalog_snapshot(
        &self,
        cmd: ActivateCatalogSnapshotCommand,
    ) -> Result<CatalogSnapshotView, AppError> {
        let now = self.clock.now();
        let event_id = EventId::from_uuid(self.ids.new_uuid());

        let view = self.store().transact(|uow| {
            idempotent(uow, cmd.request_id, "ActivateCatalogSnapshot", now, |uow| {
                let device = require_device(uow)?;
                let mut snapshot = load_snapshot(uow, cmd.snapshot_id)?;

                // Already in force: nothing to do, and no second journal entry.
                if snapshot.status() == CatalogSnapshotStatus::Active {
                    let stored = uow.catalog_stored_counts(cmd.snapshot_id)?;
                    return Ok(CatalogSnapshotView { snapshot, stored });
                }

                // **Before anything is dismounted.** A snapshot that
                // references bytes this installation does not have, or has
                // wrong, never becomes the catalog that draws a menu — and the
                // one already in force keeps selling (ADR 045).
                //
                // Inside the transaction on purpose: there is then no instant
                // between "the assets are there" and "this is the catalog".
                self.require_assets_ready(uow, cmd.snapshot_id)?;

                let superseded = match uow.find_active_catalog_snapshot(&device.store_id)? {
                    Some(mut current) => {
                        current.supersede(now)?;
                        uow.update_catalog_snapshot(&current, now)?;
                        Some(current.id())
                    }
                    None => None,
                };

                snapshot.activate(now)?;
                uow.update_catalog_snapshot(&snapshot, now)?;
                let stored = uow.catalog_stored_counts(cmd.snapshot_id)?;

                let ctx = EventContext {
                    device: &device,
                    register_id: None,
                    business_day_id: None,
                    session_id: None,
                    transaction_id: None,
                    operator_id: None,
                };
                uow.append_journal(&journal_event(
                    event_id,
                    JournalEventType::CatalogSnapshotActivated,
                    snapshot.id().to_string(),
                    &ctx,
                    now,
                    json!({
                        "version": snapshot.version(),
                        "content_hash": snapshot.state().content_hash,
                        "products": stored.products,
                        "superseded_snapshot_id": superseded.map(|id| id.to_string()),
                    }),
                ))?;
                Ok(CatalogSnapshotView { snapshot, stored })
            })
        })?;
        tracing::info!(
            snapshot_id = %view.snapshot.id(),
            version = view.snapshot.version(),
            "catalog snapshot activated"
        );
        Ok(view)
    }

    // ----------------------------------------------------------------- queries

    pub fn get_catalog_status(&self) -> Result<CatalogStatus, AppError> {
        self.store().transact(|uow| {
            let device = require_device(uow)?;
            let mut status = CatalogStatus {
                active: None,
                pending: Vec::new(),
                importing: Vec::new(),
                highest_version: uow.highest_catalog_version(&device.store_id)?,
            };
            for snapshot in uow.list_catalog_snapshots(&device.store_id, 20)? {
                let stored = uow.catalog_stored_counts(snapshot.id())?;
                let view = CatalogSnapshotView { snapshot, stored };
                match view.snapshot.status() {
                    CatalogSnapshotStatus::Active => status.active = Some(view),
                    CatalogSnapshotStatus::Pending => status.pending.push(view),
                    CatalogSnapshotStatus::Importing => status.importing.push(view),
                    CatalogSnapshotStatus::Superseded => {}
                }
            }
            Ok(status)
        })
    }

    pub fn get_catalog_import_status(
        &self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<CatalogImportStatus, AppError> {
        self.store().transact(move |uow| {
            let snapshot = load_snapshot(uow, snapshot_id)?;
            let progress = uow.catalog_import_progress(&snapshot)?;
            Ok(CatalogImportStatus { snapshot, progress })
        })
    }

    /// Diagnostic lookup against the snapshot that would price a new sale, or
    /// against the one an open sale captured.
    pub fn lookup_product(&self, selector: ProductSelector) -> Result<ProductLookup, AppError> {
        let now = self.clock.now();
        self.store().transact(move |uow| {
            let device = require_device(uow)?;

            // Prefer the snapshot an open sale is bound to, so a lookup shows
            // the operator the same price the sale would use.
            let snapshot = match require_open_business_day(uow, &device.store_id).ok() {
                Some(day) => {
                    let session = uow.find_open_sessions(day.id())?.into_iter().next();
                    let active = match &session {
                        Some(session) => uow.find_active_transaction(session.id())?,
                        None => None,
                    };
                    match active.and_then(|transaction| transaction.pricing()) {
                        Some(captured) => uow
                            .find_catalog_snapshot(captured.snapshot_id)?
                            .filter(CatalogSnapshot::resolves_sales)
                            .map(|snapshot| (snapshot, captured.effective_at)),
                        None => None,
                    }
                }
                None => None,
            };
            // An open sale's own instant, so the operator sees the price that
            // sale would use; otherwise now.
            let (snapshot, effective_at) = match snapshot {
                Some(found) => found,
                None => (
                    uow.find_active_catalog_snapshot(&device.store_id)?
                        .ok_or(CatalogError::NoActiveSnapshot)?,
                    now,
                ),
            };

            let product = uow
                .lookup_catalog_product(snapshot.id(), &selector)?
                .ok_or_else(|| CatalogError::ProductNotFound {
                    selector: selector.to_string(),
                })?;
            let tax_profile = uow
                .find_catalog_tax_profile(snapshot.id(), &product.tax_profile)?
                .ok_or_else(|| CatalogError::UnknownTaxProfile {
                    product_ref: product.product_ref.to_string(),
                    code: product.tax_profile.to_string(),
                })?;
            let context = PricingContext {
                store_id: &device.store_id,
                product_ref: &product.product_ref,
                effective_at,
                snapshot_id: snapshot.id(),
                quantity: Quantity::from_units(1)
                    .map_err(|e| AppError::Catalog(CatalogError::Money(e)))?,
                currency: device.currency,
            };
            let candidates =
                uow.price_candidates(snapshot.id(), &device.store_id, &product.product_ref)?;
            let (price, price_issue) = match resolve_price(&candidates, &context) {
                Ok(price) => (Some(price), None),
                Err(error) => (None, Some(error.code().to_owned())),
            };

            Ok(ProductLookup {
                snapshot_id: snapshot.id(),
                snapshot_version: snapshot.version(),
                effective_at,
                product,
                tax_profile,
                price,
                price_issue,
            })
        })
    }
}

fn load_snapshot(
    uow: &mut dyn UnitOfWork,
    snapshot_id: CatalogSnapshotId,
) -> Result<CatalogSnapshot, AppError> {
    uow.find_catalog_snapshot(snapshot_id)?.ok_or_else(|| {
        AppError::Catalog(CatalogError::SnapshotNotFound {
            snapshot_id: snapshot_id.to_string(),
        })
    })
}

/// Structural checks that only a complete snapshot can answer (ADR 044).
///
/// A chunk cannot decide these on its own: a selection group may hang from an
/// option delivered in another chunk, an availability row may name a product
/// that arrives later, and a cycle can be closed by the last chunk of all.
/// Foreign keys catch what is local; this catches the rest, and it runs before
/// the snapshot is marked `PENDING`, so an invalid structure never becomes
/// usable.
///
/// Costs nothing for a Retail catalog: with no extensions stored there is
/// nothing to load and the function returns immediately.
fn verify_catalog_structure(
    uow: &mut dyn UnitOfWork,
    snapshot_id: CatalogSnapshotId,
    stored: SnapshotCounts,
) -> Result<(), AppError> {
    if stored.channels == 0
        && stored.categories == 0
        && stored.presentation == 0
        && stored.selection_groups == 0
        && stored.selection_options == 0
        && stored.availability == 0
    {
        return Ok(());
    }

    let channels = uow.list_catalog_channels(snapshot_id)?;
    let availability = uow.list_catalog_availability(snapshot_id)?;
    validate_channels(&channels, &availability)?;

    let groups = uow.list_catalog_selection_groups(snapshot_id)?;
    let options = uow.list_catalog_selection_options(snapshot_id)?;
    // Only the product references the extensions actually name: validating a
    // menu must not load a catalog.
    let products = uow.list_catalog_extension_product_refs(snapshot_id)?;
    validate_selection_structure(&groups, &options, &products)?;

    // One hash is one set of bytes, so one size and one media type. A
    // snapshot that says otherwise has no answer to "how big is this asset?",
    // and its manifest would be ambiguous (ADR 045).
    let assets = uow.list_catalog_required_assets(snapshot_id)?;
    for pair in assets.windows(2) {
        if pair[0].content_hash == pair[1].content_hash {
            return Err(CatalogError::ConflictingAssetDeclaration {
                content_hash: pair[0].content_hash.clone(),
            }
            .into());
        }
    }

    let categories: Vec<CategoryRef> = uow
        .list_catalog_categories(snapshot_id)?
        .into_iter()
        .map(|category| category.category_ref)
        .collect();
    let option_ids: Vec<SelectionOptionId> = options
        .iter()
        .map(|option| option.option_id.clone())
        .collect();
    validate_availability_targets(&availability, &products, &categories, &option_ids)?;
    Ok(())
}

/// The money a rule carries, so the import can check it against the snapshot's
/// currency. A percentage or a buy-N-get-M carries none.
fn promotion_amounts(rule: &PromotionRule) -> Vec<Money> {
    match rule {
        PromotionRule::PercentageOff { .. } | PromotionRule::BuyNGetM { .. } => Vec::new(),
        PromotionRule::FixedAmountOff { amount, .. } => vec![*amount],
        PromotionRule::BasketThreshold { minimum, amount } => vec![*minimum, *amount],
    }
}
