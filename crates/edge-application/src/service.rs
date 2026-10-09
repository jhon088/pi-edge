//! The use cases.
//!
//! `EdgeApplication` is the single entry point of the headless core. It is
//! generic over its ports so the composition root (CLI today, daemon or Tauri
//! host tomorrow) chooses the concrete adapters.
//!
//! Context resolution rule: PI.Edge runs on one terminal, so commands act on
//! *the* open business day of the device's store, *the* open session of that
//! business day and *the* active transaction of that session. Ambiguity is an
//! error, never a guess.

use edge_catalog::basket::{
    evaluate, AdjustmentTarget, BasketLine, NewBasketLine, PricedBasket, PromotionContext,
};
use edge_catalog::{resolve_price, resolve_sale_line, CatalogSnapshot, PricingContext};
use edge_domain::prelude::*;
use edge_payments::{PaymentProviderPort, SimulatorPaymentProvider};
use serde_json::json;

use crate::commands::*;
use crate::context::{
    idempotent, journal_event, require_active_transaction, require_device,
    require_open_business_day, require_single_open_session, EventContext,
};
use crate::error::AppError;
use crate::ports::{Clock, DeviceProfile, EdgeStore, IdFactory, UnitOfWork};
use crate::queries::{EdgeState, PendingOutbox};
use crate::CORE_VERSION;

/// Headless POS core.
///
/// `P` is the payment provider the composition root chose. It is a type
/// parameter rather than a trait object so the core stays monomorphic and a
/// test can script a provider without any dynamic dispatch (ADR 020).
#[derive(Debug)]
pub struct EdgeApplication<S, C, I, P = SimulatorPaymentProvider> {
    store: S,
    pub(crate) clock: C,
    pub(crate) ids: I,
    pub(crate) payments: P,
}

impl<S, C, I> EdgeApplication<S, C, I, SimulatorPaymentProvider>
where
    S: EdgeStore,
    C: Clock,
    I: IdFactory,
{
    /// Core with an approving payment simulator. There is no real provider
    /// yet, so this is what every composition root uses today; a scripted one
    /// goes through [`EdgeApplication::with_payment_provider`].
    pub fn new(store: S, clock: C, ids: I) -> Self {
        Self::with_payment_provider(store, clock, ids, SimulatorPaymentProvider::approving())
    }
}

impl<S, C, I, P> EdgeApplication<S, C, I, P>
where
    S: EdgeStore,
    C: Clock,
    I: IdFactory,
    P: PaymentProviderPort,
{
    pub fn with_payment_provider(store: S, clock: C, ids: I, payments: P) -> Self {
        Self {
            store,
            clock,
            ids,
            payments,
        }
    }

    /// The provider this core settles provider-backed tenders through.
    pub fn payment_provider(&self) -> &P {
        &self.payments
    }

    /// Access to the store, for adapters that need the same transaction
    /// boundary (tests, future dispatcher).
    pub fn store(&self) -> &S {
        &self.store
    }

    fn new_event_id(&self) -> EventId {
        EventId::from_uuid(self.ids.new_uuid())
    }

    // ---------------------------------------------------------------- commands

    /// Provision the device identity. Idempotent by nature: an existing
    /// profile is returned untouched.
    pub fn init_device(&self, cmd: InitDeviceCommand) -> Result<DeviceProfile, AppError> {
        let now = self.clock.now();
        let profile = self.store.transact(move |uow| {
            if let Some(existing) = uow.device_profile()? {
                return Ok(existing);
            }
            let profile = DeviceProfile {
                tenant_id: cmd.tenant_id,
                store_id: cmd.store_id,
                currency: cmd.currency,
                allow_negative_inventory: cmd.allow_negative_inventory,
            };
            uow.put_device_profile(&profile, now)?;
            Ok(profile)
        })?;
        tracing::info!(
            tenant_id = %profile.tenant_id,
            store_id = %profile.store_id,
            currency = %profile.currency,
            "device profile ready"
        );
        Ok(profile)
    }

    pub fn open_business_day(&self, cmd: OpenBusinessDayCommand) -> Result<BusinessDay, AppError> {
        let now = self.clock.now();
        let business_day_id = BusinessDayId::from_uuid(self.ids.new_uuid());
        let event_id = self.new_event_id();

        let day = self.store.transact(move |uow| {
            idempotent(uow, cmd.request_id, "OpenBusinessDay", now, |uow| {
                let device = require_device(uow)?;
                if let Some(existing) = uow.find_open_business_day(&device.store_id)? {
                    return Err(AppError::BusinessDayAlreadyOpen {
                        business_day_id: existing.id().to_string(),
                    });
                }
                let day = BusinessDay::open(NewBusinessDay {
                    business_day_id,
                    tenant_id: device.tenant_id.clone(),
                    store_id: device.store_id.clone(),
                    opened_at: now,
                });
                uow.insert_business_day(&day, now)?;
                let ctx = EventContext {
                    device: &device,
                    register_id: None,
                    business_day_id: Some(day.id()),
                    session_id: None,
                    transaction_id: None,
                    operator_id: None,
                };
                uow.append_journal(&journal_event(
                    event_id,
                    JournalEventType::BusinessDayOpened,
                    day.id().to_string(),
                    &ctx,
                    now,
                    json!({
                        "opened_at": now.to_rfc3339(),
                        "version": day.state().version,
                    }),
                ))?;
                Ok(day)
            })
        })?;
        tracing::info!(business_day_id = %day.id(), "business day opened");
        Ok(day)
    }

    pub fn open_register_session(
        &self,
        cmd: OpenRegisterSessionCommand,
    ) -> Result<RegisterSession, AppError> {
        let now = self.clock.now();
        let session_id = SessionId::from_uuid(self.ids.new_uuid());
        let opening_float_movement_id = self.ids.new_uuid();
        let event_id = self.new_event_id();

        let session = self.store.transact(move |uow| {
            idempotent(uow, cmd.request_id, "OpenRegisterSession", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                require_business_day_open(&day)?;
                if let Some(existing) = uow.find_open_session_for_register(&cmd.register_id)? {
                    return Err(AppError::RegisterSessionAlreadyOpen {
                        register_id: cmd.register_id.to_string(),
                        session_id: existing.id().to_string(),
                    });
                }
                let session = RegisterSession::open(NewRegisterSession {
                    reopened_from_session_id: None,
                    session_id,
                    business_day_id: day.id(),
                    register_id: cmd.register_id.clone(),
                    operator_id: cmd.operator_id.clone(),
                    opening_cash: Money::from_minor(cmd.opening_cash_minor, device.currency),
                    opened_at: now,
                })?;
                uow.insert_register_session(&session, now)?;
                // The float becomes the ledger's first movement, so the drawer
                // is explained by the ledger alone (ADR 021).
                crate::cash_service::record_opening_float(
                    uow,
                    opening_float_movement_id,
                    &day,
                    &session,
                    now,
                )?;
                let ctx = EventContext {
                    device: &device,
                    register_id: Some(&cmd.register_id),
                    business_day_id: Some(day.id()),
                    session_id: Some(session.id()),
                    transaction_id: None,
                    operator_id: Some(&cmd.operator_id),
                };
                uow.append_journal(&journal_event(
                    event_id,
                    JournalEventType::SessionOpened,
                    session.id().to_string(),
                    &ctx,
                    now,
                    json!({
                        "opening_cash_minor": cmd.opening_cash_minor,
                        "currency": device.currency.code(),
                        "opened_at": now.to_rfc3339(),
                    }),
                ))?;
                Ok(session)
            })
        })?;
        tracing::info!(
            session_id = %session.id(),
            register_id = %session.state().register_id,
            "register session opened"
        );
        Ok(session)
    }

    pub fn start_transaction(
        &self,
        cmd: StartTransactionCommand,
    ) -> Result<RetailTransaction, AppError> {
        let now = self.clock.now();
        let transaction_id = TransactionId::from_uuid(self.ids.new_uuid());
        let event_id = self.new_event_id();

        let transaction = self.store.transact(move |uow| {
            idempotent(uow, cmd.request_id, "StartTransaction", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                require_business_day_open(&day)?;
                let session = require_single_open_session(uow, day.id())?;
                require_session_open(&session)?;
                if let Some(active) = uow.find_active_transaction(session.id())? {
                    return Err(AppError::TransactionAlreadyActive {
                        transaction_id: active.id().to_string(),
                    });
                }
                // A sale cannot start without a catalog to price it, and it
                // keeps the snapshot it captured here for its whole life.
                let catalog = require_active_catalog(uow, &device.store_id)?;
                let register_id = session.state().register_id.clone();
                let register_sequence = uow.next_register_sequence(&register_id)?;
                let transaction = RetailTransaction::start(NewRetailTransaction {
                    transaction_id,
                    business_day_id: day.id(),
                    session_id: session.id(),
                    store_id: device.store_id.clone(),
                    register_id: register_id.clone(),
                    operator_id: session.state().operator_id.clone(),
                    register_sequence,
                    currency: device.currency,
                    // Pin both the catalog and the instant: a scheduled price
                    // change must not move a price mid-sale either (ADR 014).
                    pricing: Some(PricingCapture {
                        snapshot_id: catalog.id(),
                        catalog_version: catalog.version(),
                        effective_at: now,
                    }),
                    created_at: now,
                });
                uow.insert_transaction(&transaction, now)?;
                let ctx = EventContext {
                    device: &device,
                    register_id: Some(&register_id),
                    business_day_id: Some(day.id()),
                    session_id: Some(session.id()),
                    transaction_id: Some(transaction.id()),
                    operator_id: Some(&session.state().operator_id),
                };
                uow.append_journal(&journal_event(
                    event_id,
                    JournalEventType::TransactionStarted,
                    transaction.id().to_string(),
                    &ctx,
                    now,
                    json!({
                        "register_sequence": register_sequence,
                        "currency": device.currency.code(),
                        "catalog_snapshot_id": catalog.id().to_string(),
                        "catalog_version": catalog.version(),
                        "pricing_effective_at": now.to_rfc3339(),
                    }),
                ))?;
                Ok(transaction)
            })
        })?;
        tracing::info!(
            transaction_id = %transaction.id(),
            register_sequence = transaction.state().register_sequence,
            "transaction started"
        );
        Ok(transaction)
    }

    pub fn add_item(&self, cmd: AddItemCommand) -> Result<RetailTransaction, AppError> {
        let now = self.clock.now();
        let line_id = LineId::from_uuid(self.ids.new_uuid());
        let event_id = self.new_event_id();

        let transaction = self.store.transact(move |uow| {
            idempotent(uow, cmd.request_id, "AddItem", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                require_business_day_open(&day)?;
                let session = require_single_open_session(uow, day.id())?;
                require_session_open(&session)?;
                let mut transaction = require_active_transaction(uow, session.id())?;

                // Resolve against the snapshot this sale captured, never
                // against whatever is active right now (ADR 013).
                let (snapshot, capture) = require_sale_pricing(uow, &transaction)?;
                let product = uow
                    .lookup_catalog_product(snapshot.id(), &cmd.selector)?
                    .ok_or_else(|| {
                        AppError::Catalog(edge_catalog::CatalogError::ProductNotFound {
                            selector: cmd.selector.to_string(),
                        })
                    })?;
                let profile = uow
                    .find_catalog_tax_profile(snapshot.id(), &product.tax_profile)?
                    .ok_or_else(|| {
                        AppError::Catalog(edge_catalog::CatalogError::UnknownTaxProfile {
                            product_ref: product.product_ref.to_string(),
                            code: product.tax_profile.to_string(),
                        })
                    })?;
                // Prices come from price books, resolved at the instant this
                // sale captured, never at "now".
                let context = PricingContext {
                    store_id: &device.store_id,
                    product_ref: &product.product_ref,
                    effective_at: capture.effective_at,
                    snapshot_id: snapshot.id(),
                    quantity: cmd.quantity,
                    currency: device.currency,
                };
                let candidates =
                    uow.price_candidates(snapshot.id(), &device.store_id, &product.product_ref)?;
                let price = resolve_price(&candidates, &context)?;
                let resolved = resolve_sale_line(
                    &product,
                    &profile,
                    &price,
                    cmd.selector.scanned_barcode(),
                    &context,
                )?;
                let line = transaction.add_line(
                    NewLineInput {
                        line_id,
                        product_ref: resolved.product_ref.clone(),
                        sku: resolved.sku.clone(),
                        barcode: resolved
                            .barcode
                            .as_ref()
                            .map(|code| code.as_str().to_owned()),
                        description: resolved.description.clone(),
                        quantity: resolved.quantity,
                        unit_price: resolved.unit_price,
                        tax_rate: resolved.tax_rate,
                        price_mode: resolved.price_mode,
                        price_book_id: Some(resolved.price_book_id.clone()),
                        // Promotions and manual overrides will supply this; no
                        // client may.
                        discount_amount: None,
                    },
                    now,
                )?;
                // The basket is re-evaluated from scratch on every change: a
                // promotion is a property of the whole basket, not of the line
                // that happened to trigger it (ADR 015).
                let promotions =
                    evaluate_basket_promotions(uow, &device, &mut transaction, &capture, now)?;
                let line = transaction
                    .state()
                    .lines
                    .iter()
                    .find(|candidate| candidate.line_number == line.line_number)
                    .cloned()
                    .ok_or(DomainError::LineNotFound {
                        line_number: line.line_number,
                    })?;
                uow.update_transaction(&transaction, now)?;
                let ctx = EventContext {
                    device: &device,
                    register_id: Some(&transaction.state().register_id),
                    business_day_id: Some(day.id()),
                    session_id: Some(session.id()),
                    transaction_id: Some(transaction.id()),
                    operator_id: Some(&transaction.state().operator_id),
                };
                uow.append_journal(&journal_event(
                    event_id,
                    JournalEventType::ItemAdded,
                    transaction.id().to_string(),
                    &ctx,
                    now,
                    json!({
                        "line_id": line.line_id.to_string(),
                        "line_number": line.line_number,
                        "product_ref": line.product_ref.as_str(),
                        "description": line.description,
                        "quantity_thousandths": line.quantity.thousandths(),
                        "unit_price_minor": line.unit_price.minor(),
                        "tax_rate_bps": line.tax_rate.bps(),
                        "price_mode": line.price_mode.as_str(),
                        "discount_minor": line.discount_amount.minor(),
                        "taxable_minor": line.taxable_amount.minor(),
                        "tax_minor": line.tax_amount.minor(),
                        "line_total_minor": line.line_total.minor(),
                        "catalog_snapshot_id": snapshot.id().to_string(),
                        "catalog_version": snapshot.version(),
                        "price_book_id": resolved.price_book_id.to_string(),
                        "priced_at": resolved.priced_at.to_rfc3339(),
                        "resolved_by": cmd.selector.to_string(),
                        "promotion_count": promotions,
                    }),
                ))?;
                Ok(transaction)
            })
        })?;
        tracing::info!(
            transaction_id = %transaction.id(),
            lines = transaction.state().lines.len(),
            "item added"
        );
        Ok(transaction)
    }

    /// Remove one line and re-evaluate the basket.
    pub fn remove_item(&self, cmd: RemoveItemCommand) -> Result<RetailTransaction, AppError> {
        let now = self.clock.now();
        let event_id = self.new_event_id();

        let transaction = self.store.transact(move |uow| {
            idempotent(uow, cmd.request_id, "RemoveItem", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                require_business_day_open(&day)?;
                let session = require_single_open_session(uow, day.id())?;
                require_session_open(&session)?;
                let mut transaction = require_active_transaction(uow, session.id())?;
                let (_, capture) = require_sale_pricing(uow, &transaction)?;

                let removed = transaction.remove_line(cmd.line_number, now)?;
                let promotions =
                    evaluate_basket_promotions(uow, &device, &mut transaction, &capture, now)?;
                uow.update_transaction(&transaction, now)?;
                let ctx = EventContext {
                    device: &device,
                    register_id: Some(&transaction.state().register_id),
                    business_day_id: Some(day.id()),
                    session_id: Some(session.id()),
                    transaction_id: Some(transaction.id()),
                    operator_id: Some(&transaction.state().operator_id),
                };
                uow.append_journal(&journal_event(
                    event_id,
                    JournalEventType::ItemRemoved,
                    transaction.id().to_string(),
                    &ctx,
                    now,
                    json!({
                        "line_id": removed.line_id.to_string(),
                        "line_number": removed.line_number,
                        "product_ref": removed.product_ref.as_str(),
                        "description": removed.description,
                        "quantity_thousandths": removed.quantity.thousandths(),
                        "line_total_minor": removed.line_total.minor(),
                        "remaining_lines": transaction.state().lines.len(),
                        "promotion_count": promotions,
                    }),
                ))?;
                Ok(transaction)
            })
        })?;
        tracing::info!(
            transaction_id = %transaction.id(),
            lines = transaction.state().lines.len(),
            "item removed"
        );
        Ok(transaction)
    }

    /// Change one line's quantity and re-evaluate the basket.
    ///
    /// The line keeps its identity and its captured price: only the quantity
    /// and everything derived from it move.
    pub fn change_quantity(
        &self,
        cmd: ChangeQuantityCommand,
    ) -> Result<RetailTransaction, AppError> {
        let now = self.clock.now();
        let event_id = self.new_event_id();

        let transaction = self.store.transact(move |uow| {
            idempotent(uow, cmd.request_id, "ChangeQuantity", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                require_business_day_open(&day)?;
                let session = require_single_open_session(uow, day.id())?;
                require_session_open(&session)?;
                let mut transaction = require_active_transaction(uow, session.id())?;
                let (_, capture) = require_sale_pricing(uow, &transaction)?;

                let previous = transaction
                    .state()
                    .lines
                    .iter()
                    .find(|line| line.line_number == cmd.line_number)
                    .map(|line| line.quantity)
                    .ok_or(DomainError::LineNotFound {
                        line_number: cmd.line_number,
                    })?;
                let changed =
                    transaction.change_line_quantity(cmd.line_number, cmd.quantity, now)?;
                let promotions =
                    evaluate_basket_promotions(uow, &device, &mut transaction, &capture, now)?;
                uow.update_transaction(&transaction, now)?;
                let ctx = EventContext {
                    device: &device,
                    register_id: Some(&transaction.state().register_id),
                    business_day_id: Some(day.id()),
                    session_id: Some(session.id()),
                    transaction_id: Some(transaction.id()),
                    operator_id: Some(&transaction.state().operator_id),
                };
                uow.append_journal(&journal_event(
                    event_id,
                    JournalEventType::LineQuantityChanged,
                    transaction.id().to_string(),
                    &ctx,
                    now,
                    json!({
                        "line_id": changed.line_id.to_string(),
                        "line_number": changed.line_number,
                        "product_ref": changed.product_ref.as_str(),
                        "from_quantity_thousandths": previous.thousandths(),
                        "to_quantity_thousandths": cmd.quantity.thousandths(),
                        "promotion_count": promotions,
                    }),
                ))?;
                Ok(transaction)
            })
        })?;
        tracing::info!(
            transaction_id = %transaction.id(),
            line_number = cmd.line_number,
            "line quantity changed"
        );
        Ok(transaction)
    }

    pub fn totalize_transaction(
        &self,
        cmd: TotalizeTransactionCommand,
    ) -> Result<RetailTransaction, AppError> {
        let now = self.clock.now();
        let event_id = self.new_event_id();

        let transaction = self.store.transact(move |uow| {
            idempotent(uow, cmd.request_id, "TotalizeTransaction", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                let session = require_single_open_session(uow, day.id())?;
                let mut transaction = require_active_transaction(uow, session.id())?;
                let totals = transaction.totalize(now)?;
                uow.update_transaction(&transaction, now)?;
                let ctx = EventContext {
                    device: &device,
                    register_id: Some(&transaction.state().register_id),
                    business_day_id: Some(day.id()),
                    session_id: Some(session.id()),
                    transaction_id: Some(transaction.id()),
                    operator_id: Some(&transaction.state().operator_id),
                };
                uow.append_journal(&journal_event(
                    event_id,
                    JournalEventType::TransactionTotalized,
                    transaction.id().to_string(),
                    &ctx,
                    now,
                    json!({
                        "subtotal_minor": totals.subtotal.minor(),
                        "discount_total_minor": totals.discount_total.minor(),
                        "net_total_minor": totals.net_total.minor(),
                        "tax_total_minor": totals.tax_total.minor(),
                        "grand_total_minor": totals.grand_total.minor(),
                        "line_count": transaction.state().lines.len(),
                        // The promotion summary is journalled here and only
                        // here: this is the moment the totals become official.
                        "promotions": transaction
                            .applied_promotions()
                            .iter()
                            .map(|applied| json!({
                                "adjustment_id": applied.adjustment_id,
                                "promotion_id": applied.promotion_id,
                                "code": applied.code,
                                "name": applied.name,
                                "priority": applied.priority,
                                "stackability": applied.stackability,
                                "target": applied.target.label(),
                                "reason": applied.reason,
                                "amount_minor": applied.amount.minor(),
                            }))
                            .collect::<Vec<_>>(),
                    }),
                ))?;
                Ok(transaction)
            })
        })?;
        tracing::info!(
            transaction_id = %transaction.id(),
            grand_total_minor = transaction.state().totals.grand_total.minor(),
            "transaction totalized"
        );
        Ok(transaction)
    }

    /// Finalize the sale. The transaction, its journal event and its outbox
    /// event are written in the same SQLite transaction; a crash before commit
    /// leaves none of them behind.
    pub fn finalize_transaction(
        &self,
        cmd: FinalizeTransactionCommand,
    ) -> Result<RetailTransaction, AppError> {
        let now = self.clock.now();
        let journal_event_id = self.new_event_id();
        let outbox_event_id = self.new_event_id();
        // One movement per line, and a sale can have any number of lines, so
        // ids are minted on demand inside the transaction.
        let ids = &self.ids;

        let transaction = self.store.transact(|uow| {
            idempotent(uow, cmd.request_id, "FinalizeTransaction", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                let session = require_single_open_session(uow, day.id())?;
                let mut transaction = require_active_transaction(uow, session.id())?;
                transaction.finalize(now)?;
                uow.update_transaction(&transaction, now)?;
                // Stock moves here and nowhere else, in the same transaction as
                // the finalization: there is no state in which a sale is
                // FINALIZED and its stock has not moved (ADR 027).
                let stock_movements = crate::inventory_service::record_sale_movements(
                    uow,
                    ids,
                    &device,
                    &transaction,
                    day.id(),
                    session.id(),
                    now,
                )?;
                let ctx = EventContext {
                    device: &device,
                    register_id: Some(&transaction.state().register_id),
                    business_day_id: Some(day.id()),
                    session_id: Some(session.id()),
                    transaction_id: Some(transaction.id()),
                    operator_id: Some(&transaction.state().operator_id),
                };
                uow.append_journal(&journal_event(
                    journal_event_id,
                    JournalEventType::TransactionFinalized,
                    transaction.id().to_string(),
                    &ctx,
                    now,
                    json!({
                        "grand_total_minor": transaction.state().totals.grand_total.minor(),
                        "paid_total_minor": transaction.paid_total()?.minor(),
                        "change_due_minor": transaction.change_due()?.minor(),
                        "register_sequence": transaction.state().register_sequence,
                        "finalized_at": now.to_rfc3339(),
                        // A count, not a list: the ledger carries the detail,
                        // and a 100-line sale must not produce 100 journal
                        // events (ADR 028).
                        "inventory_movements": stock_movements,
                    }),
                ))?;
                let payload = serde_json::to_value(transaction.state())
                    .map_err(|e| AppError::Serialization(e.to_string()))?;
                uow.enqueue_outbox(&OutboxEvent::pending(
                    outbox_event_id,
                    OutboxEventType::SaleFinalized,
                    transaction.id().to_string(),
                    transaction.state().version,
                    device.tenant_id.clone(),
                    device.store_id.clone(),
                    Some(transaction.state().register_id.clone()),
                    Some(day.id()),
                    now,
                    payload,
                ))?;
                Ok(transaction)
            })
        })?;
        tracing::info!(
            transaction_id = %transaction.id(),
            grand_total_minor = transaction.state().totals.grand_total.minor(),
            "transaction finalized"
        );
        Ok(transaction)
    }

    // `close_register_session` is gone. A session now closes through the blind
    // close in `cash_service.rs`: `request_session_close` then
    // `declare_closing_cash` (ADR 022). A single-shot close would let a client
    // finish a till without ever counting it.

    pub fn close_business_day(
        &self,
        cmd: CloseBusinessDayCommand,
    ) -> Result<BusinessDay, AppError> {
        let now = self.clock.now();
        let journal_event_id = self.new_event_id();
        let outbox_event_id = self.new_event_id();

        let day = self.store.transact(move |uow| {
            idempotent(uow, cmd.request_id, "CloseBusinessDay", now, |uow| {
                let device = require_device(uow)?;
                let mut day = require_open_business_day(uow, &device.store_id)?;
                // Everything standing in the way, named together: a drawer
                // nobody counted, a count nobody declared, a sale in progress,
                // money whose fate or location is unknown (ADR 024).
                let mut blockers = Vec::new();
                for session in uow.find_open_sessions(day.id())? {
                    blockers.push(format!(
                        "{}:{}",
                        if session.is_close_requested() {
                            "SESSION_AWAITING_DECLARATION"
                        } else {
                            "OPEN_SESSION"
                        },
                        session.id()
                    ));
                    blockers.extend(crate::cash_service::close_blockers(uow, &session)?);
                }
                if !blockers.is_empty() {
                    return Err(AppError::BusinessDayCloseBlocked { blockers });
                }
                day.close(now)?;
                uow.update_business_day(&day, now)?;
                let ctx = EventContext {
                    device: &device,
                    register_id: None,
                    business_day_id: Some(day.id()),
                    session_id: None,
                    transaction_id: None,
                    operator_id: None,
                };
                uow.append_journal(&journal_event(
                    journal_event_id,
                    JournalEventType::BusinessDayClosed,
                    day.id().to_string(),
                    &ctx,
                    now,
                    json!({ "closed_at": now.to_rfc3339() }),
                ))?;
                let payload = serde_json::to_value(day.state())
                    .map_err(|e| AppError::Serialization(e.to_string()))?;
                uow.enqueue_outbox(&OutboxEvent::pending(
                    outbox_event_id,
                    OutboxEventType::BusinessDayClosed,
                    day.id().to_string(),
                    day.state().version,
                    device.tenant_id.clone(),
                    device.store_id.clone(),
                    None,
                    Some(day.id()),
                    now,
                    payload,
                ))?;
                Ok(day)
            })
        })?;
        tracing::info!(business_day_id = %day.id(), "business day closed");
        Ok(day)
    }

    // ----------------------------------------------------------------- queries

    /// The device identity. Cheap single-row read, used by adapters that must
    /// validate a payload against the store's currency.
    pub fn get_device(&self) -> Result<DeviceProfile, AppError> {
        self.store.transact(|uow| require_device(uow))
    }

    /// Full recovery query: rebuilds the operational state from SQLite only.
    pub fn get_edge_state(&self) -> Result<EdgeState, AppError> {
        self.store.transact(|uow| {
            let device = uow.device_profile()?;
            let mut business_day = None;
            let mut session = None;
            let mut active_transaction = None;
            if let Some(device) = &device {
                business_day = uow.find_open_business_day(&device.store_id)?;
                if let Some(day) = &business_day {
                    session = uow.find_open_sessions(day.id())?.into_iter().next();
                    if let Some(open_session) = &session {
                        active_transaction = uow.find_active_transaction(open_session.id())?;
                    }
                }
            }
            Ok(EdgeState {
                core_version: CORE_VERSION.to_owned(),
                device,
                business_day,
                session,
                active_transaction,
                pending_outbox: uow.count_pending_outbox()?,
            })
        })
    }

    pub fn get_active_transaction(&self) -> Result<Option<RetailTransaction>, AppError> {
        self.store.transact(|uow| {
            let device = require_device(uow)?;
            let Some(day) = uow.find_open_business_day(&device.store_id)? else {
                return Ok(None);
            };
            let Some(session) = uow.find_open_sessions(day.id())?.into_iter().next() else {
                return Ok(None);
            };
            uow.find_active_transaction(session.id())
        })
    }

    /// Read one sale back by id, finalized ones included.
    ///
    /// What the sale granted is materialised on it, so this stays readable
    /// after the catalog snapshot that produced the promotions is gone.
    pub fn get_transaction(
        &self,
        transaction_id: TransactionId,
    ) -> Result<Option<RetailTransaction>, AppError> {
        self.store
            .transact(move |uow| uow.find_transaction(transaction_id))
    }

    pub fn get_pending_outbox(&self, limit: i64) -> Result<PendingOutbox, AppError> {
        self.store.transact(move |uow| {
            Ok(PendingOutbox {
                pending_count: uow.count_pending_outbox()?,
                events: uow.list_pending_outbox(limit)?,
            })
        })
    }

    pub fn get_journal(&self, limit: i64) -> Result<Vec<JournalEvent>, AppError> {
        self.store.transact(move |uow| uow.list_journal(limit))
    }
}

fn require_business_day_open(day: &BusinessDay) -> Result<(), AppError> {
    if day.is_open() {
        Ok(())
    } else {
        Err(AppError::BusinessDayNotOpen {
            status: day.status().as_str(),
        })
    }
}

fn require_session_open(session: &RegisterSession) -> Result<(), AppError> {
    if session.is_open() {
        Ok(())
    } else {
        Err(AppError::RegisterSessionNotOpen {
            status: session.status().as_str(),
        })
    }
}

/// The snapshot a new sale must capture.
fn require_active_catalog(
    uow: &mut dyn UnitOfWork,
    store_id: &StoreId,
) -> Result<CatalogSnapshot, AppError> {
    uow.find_active_catalog_snapshot(store_id)?
        .ok_or(AppError::Catalog(
            edge_catalog::CatalogError::NoActiveSnapshot,
        ))
}

/// The pricing context an existing sale must keep using.
///
/// The snapshot is reloaded by id and required to still be usable: an import in
/// flight can never price a line, and a superseded snapshot still can.
fn require_sale_pricing(
    uow: &mut dyn UnitOfWork,
    transaction: &RetailTransaction,
) -> Result<(CatalogSnapshot, PricingCapture), AppError> {
    let captured = transaction.pricing().ok_or(AppError::Catalog(
        edge_catalog::CatalogError::NoActiveSnapshot,
    ))?;
    let snapshot = uow
        .find_catalog_snapshot(captured.snapshot_id)?
        .ok_or_else(|| {
            AppError::Catalog(edge_catalog::CatalogError::SnapshotNotFound {
                snapshot_id: captured.snapshot_id.to_string(),
            })
        })?;
    if !snapshot.resolves_sales() {
        return Err(AppError::Catalog(
            edge_catalog::CatalogError::ImportNotOpen {
                snapshot_id: snapshot.id().to_string(),
                status: snapshot.status().as_str(),
            },
        ));
    }
    Ok((snapshot, captured))
}

/// Re-evaluate every promotion against the whole basket and write the result
/// onto the transaction. Returns how many promotions applied.
///
/// Always a full re-evaluation: incremental promotion state would have to be
/// invalidated on every basket change, and getting that wrong means charging a
/// customer the wrong amount. Idempotent by construction — the same basket and
/// the same snapshot produce the same adjustments, with the same ids.
fn evaluate_basket_promotions(
    uow: &mut dyn UnitOfWork,
    device: &DeviceProfile,
    transaction: &mut RetailTransaction,
    capture: &PricingCapture,
    now: Timestamp,
) -> Result<usize, AppError> {
    let lines = transaction
        .state()
        .lines
        .iter()
        .map(|line| {
            BasketLine::new(NewBasketLine {
                line_number: line.line_number,
                product_ref: line.product_ref.clone(),
                quantity: line.quantity,
                unit_price: line.unit_price,
                price_mode: line.price_mode,
            })
            .map_err(AppError::Catalog)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let basket = PricedBasket::new(lines);

    let candidates = uow.promotion_candidates(capture.snapshot_id, &device.store_id)?;
    let result = evaluate(
        &basket,
        &candidates,
        &PromotionContext {
            store_id: &device.store_id,
            effective_at: capture.effective_at,
            snapshot_id: capture.snapshot_id,
            currency: device.currency,
        },
    )
    .map_err(AppError::Catalog)?;

    let outcome = PromotionOutcome {
        line_discounts: result
            .line_discounts
            .iter()
            .map(|discount| LineDiscount {
                line_number: discount.line_number,
                amount: discount.amount,
            })
            .collect(),
        applied: result
            .adjustments()
            .map(|adjustment| AppliedPromotion {
                adjustment_id: adjustment.adjustment_id.clone(),
                promotion_id: adjustment.promotion_id.to_string(),
                code: adjustment.promotion_code.clone(),
                name: adjustment.promotion_name.clone(),
                priority: adjustment.priority,
                stackability: adjustment.stackability.as_str().to_owned(),
                target: match adjustment.target {
                    AdjustmentTarget::Line { line_number } => PromotionTarget::Line { line_number },
                    AdjustmentTarget::Basket => PromotionTarget::Basket,
                },
                amount: adjustment.amount,
                reason: adjustment.reason_code.to_owned(),
                detail: adjustment.detail.clone(),
            })
            .collect(),
    };
    let applied = outcome.applied.len();
    transaction.apply_promotions(outcome, now)?;
    Ok(applied)
}
