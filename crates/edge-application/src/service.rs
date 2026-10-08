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

use edge_catalog::{resolve_sale_line, BasePriceResolver, CatalogSnapshot, PricingContext};
use edge_domain::prelude::*;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use crate::commands::*;
use crate::error::AppError;
use crate::ports::{Clock, DeviceProfile, EdgeStore, IdFactory, UnitOfWork};
use crate::queries::{EdgeState, PendingOutbox};
use crate::CORE_VERSION;

/// Headless POS core.
#[derive(Debug)]
pub struct EdgeApplication<S, C, I> {
    store: S,
    pub(crate) clock: C,
    pub(crate) ids: I,
}

impl<S, C, I> EdgeApplication<S, C, I>
where
    S: EdgeStore,
    C: Clock,
    I: IdFactory,
{
    pub fn new(store: S, clock: C, ids: I) -> Self {
        Self { store, clock, ids }
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
                    session_id,
                    business_day_id: day.id(),
                    register_id: cmd.register_id.clone(),
                    operator_id: cmd.operator_id.clone(),
                    opening_cash: Money::from_minor(cmd.opening_cash_minor, device.currency),
                    opened_at: now,
                })?;
                uow.insert_register_session(&session, now)?;
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
                    catalog: Some(CatalogRef {
                        snapshot_id: catalog.id(),
                        version: catalog.version(),
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
                let snapshot = require_sale_catalog(uow, &transaction)?;
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
                let resolved = resolve_sale_line(
                    &product,
                    &profile,
                    cmd.selector.scanned_barcode(),
                    &BasePriceResolver,
                    &PricingContext {
                        store_id: &device.store_id,
                        business_day_id: day.id(),
                        snapshot_version: snapshot.version(),
                        quantity: cmd.quantity,
                        currency: device.currency,
                    },
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
                        // Promotions and manual overrides will supply this; no
                        // client may.
                        discount_amount: None,
                    },
                    now,
                )?;
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
                        "resolved_by": cmd.selector.to_string(),
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

    pub fn commit_cash_payment(
        &self,
        cmd: CommitCashPaymentCommand,
    ) -> Result<RetailTransaction, AppError> {
        let now = self.clock.now();
        let payment_id = PaymentId::from_uuid(self.ids.new_uuid());
        let event_id = self.new_event_id();

        let transaction = self.store.transact(move |uow| {
            idempotent(uow, cmd.request_id, "CommitCashPayment", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                let session = require_single_open_session(uow, day.id())?;
                let mut transaction = require_active_transaction(uow, session.id())?;
                let payment = transaction.commit_cash_payment(
                    payment_id,
                    Money::from_minor(cmd.amount_minor, device.currency),
                    now,
                )?;
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
                    JournalEventType::CashPaymentCommitted,
                    transaction.id().to_string(),
                    &ctx,
                    now,
                    json!({
                        "payment_id": payment.payment_id.to_string(),
                        "tender_type": payment.tender_type.as_str(),
                        "amount_minor": payment.amount.minor(),
                        "paid_total_minor": transaction.paid_total()?.minor(),
                        "balance_due_minor": transaction.balance_due()?.minor(),
                        "change_due_minor": transaction.change_due()?.minor(),
                        "transaction_status": transaction.status().as_str(),
                    }),
                ))?;
                Ok(transaction)
            })
        })?;
        tracing::info!(
            transaction_id = %transaction.id(),
            status = transaction.status().as_str(),
            "cash payment committed"
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

        let transaction = self.store.transact(move |uow| {
            idempotent(uow, cmd.request_id, "FinalizeTransaction", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                let session = require_single_open_session(uow, day.id())?;
                let mut transaction = require_active_transaction(uow, session.id())?;
                transaction.finalize(now)?;
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

    pub fn close_register_session(
        &self,
        cmd: CloseRegisterSessionCommand,
    ) -> Result<RegisterSession, AppError> {
        let now = self.clock.now();
        let journal_event_id = self.new_event_id();
        let outbox_event_id = self.new_event_id();

        let session = self.store.transact(move |uow| {
            idempotent(uow, cmd.request_id, "CloseRegisterSession", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                let mut session = require_single_open_session(uow, day.id())?;
                if let Some(active) = uow.find_active_transaction(session.id())? {
                    return Err(AppError::SessionHasActiveTransaction {
                        transaction_id: active.id().to_string(),
                    });
                }
                session.close(now)?;
                uow.update_register_session(&session, now)?;
                let ctx = EventContext {
                    device: &device,
                    register_id: Some(&session.state().register_id),
                    business_day_id: Some(day.id()),
                    session_id: Some(session.id()),
                    transaction_id: None,
                    operator_id: Some(&session.state().operator_id),
                };
                uow.append_journal(&journal_event(
                    journal_event_id,
                    JournalEventType::SessionClosed,
                    session.id().to_string(),
                    &ctx,
                    now,
                    json!({
                        "closed_at": now.to_rfc3339(),
                        "opening_cash_minor": session.state().opening_cash.minor(),
                    }),
                ))?;
                let payload = serde_json::to_value(session.state())
                    .map_err(|e| AppError::Serialization(e.to_string()))?;
                uow.enqueue_outbox(&OutboxEvent::pending(
                    outbox_event_id,
                    OutboxEventType::RegisterSessionClosed,
                    session.id().to_string(),
                    session.state().version,
                    device.tenant_id.clone(),
                    device.store_id.clone(),
                    Some(session.state().register_id.clone()),
                    Some(day.id()),
                    now,
                    payload,
                ))?;
                Ok(session)
            })
        })?;
        tracing::info!(session_id = %session.id(), "register session closed");
        Ok(session)
    }

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
                let open_sessions = uow.find_open_sessions(day.id())?;
                if !open_sessions.is_empty() {
                    return Err(AppError::BusinessDayHasOpenSessions {
                        count: open_sessions.len(),
                    });
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

// -------------------------------------------------------------------- helpers

/// Replay guard. The lookup, the work and the result record all happen inside
/// the caller's transaction, so a crash can never record a command whose
/// effects were rolled back.
pub(crate) fn idempotent<T, F>(
    uow: &mut dyn UnitOfWork,
    request_id: Option<Uuid>,
    command_type: &'static str,
    now: Timestamp,
    work: F,
) -> Result<T, AppError>
where
    T: Serialize + DeserializeOwned,
    F: FnOnce(&mut dyn UnitOfWork) -> Result<T, AppError>,
{
    let Some(request_id) = request_id else {
        return work(uow);
    };
    if let Some(recorded) = uow.find_command_result(request_id)? {
        tracing::info!(%request_id, command_type, "replaying recorded command result");
        return serde_json::from_str(&recorded).map_err(|e| AppError::Serialization(e.to_string()));
    }
    let outcome = work(&mut *uow)?;
    let serialized =
        serde_json::to_string(&outcome).map_err(|e| AppError::Serialization(e.to_string()))?;
    uow.record_command_result(request_id, command_type, &serialized, now)?;
    Ok(outcome)
}

pub(crate) struct EventContext<'a> {
    pub(crate) device: &'a DeviceProfile,
    pub(crate) register_id: Option<&'a RegisterId>,
    pub(crate) business_day_id: Option<BusinessDayId>,
    pub(crate) session_id: Option<SessionId>,
    pub(crate) transaction_id: Option<TransactionId>,
    pub(crate) operator_id: Option<&'a OperatorId>,
}

pub(crate) fn journal_event(
    event_id: EventId,
    event_type: JournalEventType,
    aggregate_id: String,
    ctx: &EventContext<'_>,
    now: Timestamp,
    payload_json: serde_json::Value,
) -> JournalEvent {
    JournalEvent {
        event_id,
        event_type,
        aggregate_id,
        tenant_id: ctx.device.tenant_id.clone(),
        store_id: ctx.device.store_id.clone(),
        register_id: ctx.register_id.cloned(),
        business_day_id: ctx.business_day_id,
        session_id: ctx.session_id,
        transaction_id: ctx.transaction_id,
        operator_id: ctx.operator_id.cloned(),
        occurred_at: now,
        payload_json,
    }
}

pub(crate) fn require_device(uow: &mut dyn UnitOfWork) -> Result<DeviceProfile, AppError> {
    uow.device_profile()?.ok_or(AppError::DeviceNotInitialized)
}

pub(crate) fn require_open_business_day(
    uow: &mut dyn UnitOfWork,
    store_id: &StoreId,
) -> Result<BusinessDay, AppError> {
    uow.find_open_business_day(store_id)?
        .ok_or(AppError::NoOpenBusinessDay)
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

fn require_single_open_session(
    uow: &mut dyn UnitOfWork,
    business_day_id: BusinessDayId,
) -> Result<RegisterSession, AppError> {
    let mut sessions = uow.find_open_sessions(business_day_id)?;
    match sessions.len() {
        0 => Err(AppError::NoOpenRegisterSession),
        1 => Ok(sessions.remove(0)),
        count => Err(AppError::AmbiguousOpenSession { count }),
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
pub(crate) fn require_active_catalog(
    uow: &mut dyn UnitOfWork,
    store_id: &StoreId,
) -> Result<CatalogSnapshot, AppError> {
    uow.find_active_catalog_snapshot(store_id)?
        .ok_or(AppError::Catalog(
            edge_catalog::CatalogError::NoActiveSnapshot,
        ))
}

/// The snapshot an existing sale was priced against.
///
/// Reloaded by id, and required to still be usable: an import in flight can
/// never price a line, and a superseded snapshot still can.
pub(crate) fn require_sale_catalog(
    uow: &mut dyn UnitOfWork,
    transaction: &RetailTransaction,
) -> Result<CatalogSnapshot, AppError> {
    let captured = transaction.catalog().ok_or(AppError::Catalog(
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
    Ok(snapshot)
}

fn require_active_transaction(
    uow: &mut dyn UnitOfWork,
    session_id: SessionId,
) -> Result<RetailTransaction, AppError> {
    uow.find_active_transaction(session_id)?
        .ok_or(AppError::NoActiveTransaction)
}
