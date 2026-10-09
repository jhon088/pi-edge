//! Inventory use cases.
//!
//! PI.Edge has durable authority over what happened at *this installation*, so
//! none of these waits for anything upstream (ADR 026). The ledger is
//! append-only and `on_hand` is derived from it, so there is no balance to
//! update and no sequence of writes that can leave a position half-moved.
//!
//! What these use cases answer is "what does this till know?" — a projection,
//! not a store-wide count.
//!
//! The one rule worth stating here rather than only in the ADR: stock moves at
//! `FINALIZED` and nowhere else, and availability is checked before money
//! moves, never after (ADR 027, ADR 029).

use edge_domain::prelude::*;
use edge_inventory::{
    on_hand, InventoryError, InventoryMovement, InventoryPosition, MovementSource, MovementType,
    NewInventoryMovement,
};
use serde_json::json;
use uuid::Uuid;

use crate::commands::{ImportInventoryBootstrapCommand, InventoryInCommand, InventoryOutCommand};
use crate::context::{idempotent, journal_event, require_device, EventContext};
use crate::error::AppError;
use crate::ports::{
    Clock, DeviceProfile, EdgeStore, IdFactory, InventoryBootstrapState, UnitOfWork,
};
use crate::queries::InventoryPositionView;
use crate::EdgeApplication;

impl<S, C, I, P> EdgeApplication<S, C, I, P>
where
    S: EdgeStore,
    C: Clock,
    I: IdFactory,
    P: edge_payments::PaymentProviderPort,
{
    /// Import the opening baseline of this installation's ledger (ADR 028).
    ///
    /// One per installation, ever. A replay of the same snapshot returns what
    /// was recorded; a different snapshot is refused, because resolving a
    /// divergence against local movements is reconciliation and there is no
    /// sync protocol to decide it.
    pub fn import_inventory_bootstrap(
        &self,
        cmd: ImportInventoryBootstrapCommand,
    ) -> Result<InventoryBootstrapState, AppError> {
        let now = self.clock.now();
        let event_id = EventId::from_uuid(self.ids.new_uuid());
        let ids = &self.ids;

        self.store().transact(|uow| {
            idempotent(
                uow,
                cmd.request_id,
                "ImportInventoryBootstrap",
                now,
                |uow| {
                    let device = require_device(uow)?;
                    let source_snapshot_id = cmd.source_snapshot_id.trim().to_owned();
                    if source_snapshot_id.is_empty() {
                        return Err(AppError::Inventory(InventoryError::InvalidRecord {
                            field: "source_snapshot_id",
                            reason: "must not be blank".to_owned(),
                        }));
                    }
                    if cmd.items.is_empty() {
                        return Err(AppError::Inventory(InventoryError::EmptyBootstrap));
                    }

                    // The same position arriving twice is a replay; a different
                    // one is a reconciliation this milestone refuses to guess at.
                    if let Some(existing) = uow.find_inventory_bootstrap(&device.store_id)? {
                        if existing.source_snapshot_id == source_snapshot_id {
                            return Ok(existing);
                        }
                        return Err(AppError::Inventory(InventoryError::AlreadyBootstrapped {
                            source_snapshot_id: existing.source_snapshot_id,
                        }));
                    }

                    // Inventory keeps no product list of its own, so the catalog
                    // has to be in place first (ADR 026).
                    let snapshot = uow
                        .find_active_catalog_snapshot(&device.store_id)?
                        .ok_or(AppError::Inventory(InventoryError::CatalogRequired))?;
                    let mut seen: Vec<&ProductRef> = Vec::with_capacity(cmd.items.len());
                    for item in &cmd.items {
                        if seen.contains(&&item.product_ref) {
                            return Err(AppError::Inventory(
                                InventoryError::DuplicateBootstrapItem {
                                    product_ref: item.product_ref.to_string(),
                                },
                            ));
                        }
                        seen.push(&item.product_ref);
                        require_known_product(uow, snapshot.id(), &item.product_ref)?;
                    }

                    // One transaction for the whole position: a crash part-way
                    // leaves no inventory at all, never half of it.
                    for item in &cmd.items {
                        let movement = InventoryMovement::record(NewInventoryMovement {
                            movement_id: ids.new_uuid(),
                            store_id: device.store_id.clone(),
                            product_ref: item.product_ref.clone(),
                            movement_type: MovementType::OpeningBalance,
                            quantity: item.quantity,
                            source: MovementSource::Bootstrap {
                                source_snapshot_id: source_snapshot_id.clone(),
                            },
                            business_day_id: None,
                            session_id: None,
                            reason_code: None,
                            occurred_at: now,
                        })
                        .map_err(AppError::Inventory)?;
                        uow.insert_inventory_movement(&movement)?;
                    }

                    let state = InventoryBootstrapState {
                        store_id: device.store_id.clone(),
                        source_snapshot_id,
                        generated_at: cmd.generated_at,
                        item_count: i64::try_from(cmd.items.len()).unwrap_or(i64::MAX),
                        bootstrapped_at: now,
                    };
                    uow.record_inventory_bootstrap(&state)?;

                    // One journal event for the operation, not one per item: the
                    // ledger already carries the detail (ADR 028).
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
                        JournalEventType::InventoryBootstrapped,
                        state.store_id.to_string(),
                        &ctx,
                        now,
                        json!({
                            "source_snapshot_id": state.source_snapshot_id,
                            "generated_at": state.generated_at.to_rfc3339(),
                            "item_count": state.item_count,
                            "catalog_version": snapshot.version(),
                        }),
                    ))?;
                    Ok(state)
                },
            )
        })
    }

    /// What a product holds, and optionally the movements behind it.
    pub fn get_inventory_position(
        &self,
        product_ref: ProductRef,
        movement_limit: i64,
    ) -> Result<InventoryPositionView, AppError> {
        self.store().transact(move |uow| {
            let device = require_device(uow)?;
            let position = uow.inventory_position(&device.store_id, &product_ref)?;
            let movements = if movement_limit > 0 {
                uow.list_inventory_movements(&device.store_id, &product_ref, movement_limit, 0)?
            } else {
                Vec::new()
            };
            Ok(InventoryPositionView {
                on_hand_display: position.on_hand_display(),
                position,
                movements,
            })
        })
    }

    /// One product's movements, oldest first.
    pub fn get_inventory_movements(
        &self,
        product_ref: ProductRef,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<InventoryMovement>, AppError> {
        let limit = limit.clamp(1, 500);
        let offset = offset.max(0);
        self.store().transact(move |uow| {
            let device = require_device(uow)?;
            uow.list_inventory_movements(&device.store_id, &product_ref, limit, offset)
        })
    }

    pub fn inventory_in(&self, cmd: InventoryInCommand) -> Result<InventoryPositionView, AppError> {
        self.record_manual_stock_movement(
            cmd.request_id,
            "InventoryIn",
            MovementType::ManualIn,
            cmd.product_ref,
            cmd.quantity,
            cmd.reason_code,
        )
    }

    pub fn inventory_out(
        &self,
        cmd: InventoryOutCommand,
    ) -> Result<InventoryPositionView, AppError> {
        self.record_manual_stock_movement(
            cmd.request_id,
            "InventoryOut",
            MovementType::ManualOut,
            cmd.product_ref,
            cmd.quantity,
            cmd.reason_code,
        )
    }

    fn record_manual_stock_movement(
        &self,
        request_id: Option<Uuid>,
        command_type: &'static str,
        movement_type: MovementType,
        product_ref: ProductRef,
        quantity: Quantity,
        reason_code: String,
    ) -> Result<InventoryPositionView, AppError> {
        let now = self.clock.now();
        let movement_id = self.ids.new_uuid();
        // The movement's source identity. Distinct from `request_id`: that one
        // makes a retried *call* harmless, this one makes a duplicated *fact*
        // impossible, and a caller that omits a request id still gets the
        // second guarantee (ADR 027).
        let source_ref = movement_id;
        let event_id = EventId::from_uuid(self.ids.new_uuid());

        self.store().transact(move |uow| {
            idempotent(uow, request_id, command_type, now, |uow| {
                let device = require_device(uow)?;
                let snapshot = uow
                    .find_active_catalog_snapshot(&device.store_id)?
                    .ok_or(AppError::Inventory(InventoryError::CatalogRequired))?;
                require_known_product(uow, snapshot.id(), &product_ref)?;

                // Operational context when the till is trading. A manual
                // movement outside trading hours is still a real movement, so
                // the absence of a day or a session is not an error.
                let (business_day_id, session_id) =
                    match uow.find_open_business_day(&device.store_id)? {
                        None => (None, None),
                        Some(day) => {
                            let session = uow
                                .find_open_sessions(day.id())?
                                .into_iter()
                                .next()
                                .map(|session| session.id());
                            (Some(day.id()), session)
                        }
                    };

                let movement = InventoryMovement::record(NewInventoryMovement {
                    movement_id,
                    store_id: device.store_id.clone(),
                    product_ref: product_ref.clone(),
                    movement_type,
                    quantity,
                    source: MovementSource::Manual {
                        request_ref: source_ref,
                    },
                    business_day_id,
                    session_id,
                    reason_code: Some(reason_code.clone()),
                    occurred_at: now,
                })
                .map_err(AppError::Inventory)?;
                uow.insert_inventory_movement(&movement)?;

                let ctx = EventContext {
                    device: &device,
                    register_id: None,
                    business_day_id,
                    session_id,
                    transaction_id: None,
                    operator_id: None,
                };
                uow.append_journal(&journal_event(
                    event_id,
                    JournalEventType::InventoryMovementRecorded,
                    movement.product_ref.to_string(),
                    &ctx,
                    now,
                    json!({
                        "movement_id": movement.movement_id.to_string(),
                        "movement_type": movement.movement_type.as_str(),
                        "direction": movement.direction.as_str(),
                        "product_ref": movement.product_ref.as_str(),
                        "quantity_thousandths": movement.quantity.thousandths(),
                        "reason_code": movement.reason_code,
                    }),
                ))?;

                let position = uow.inventory_position(&device.store_id, &product_ref)?;
                Ok(InventoryPositionView {
                    on_hand_display: position.on_hand_display(),
                    position,
                    movements: Vec::new(),
                })
            })
        })
    }
}

/// Write one `SALE` movement per line of a finalized sale (ADR 027).
///
/// Called inside the finalization transaction, so there is no state in which a
/// sale is `FINALIZED` and its stock has not moved. `source_ref` is
/// `transaction:line`, which makes a replayed finalization a no-op rather than
/// a double deduction.
///
/// Nothing is checked here. Availability is enforced before money moves; once
/// the goods have left the counter the ledger's job is to record it, not to
/// refuse (ADR 029).
pub(crate) fn record_sale_movements(
    uow: &mut dyn UnitOfWork,
    ids: &dyn IdFactoryRef,
    device: &DeviceProfile,
    transaction: &RetailTransaction,
    business_day_id: BusinessDayId,
    session_id: SessionId,
    now: Timestamp,
) -> Result<usize, AppError> {
    let state = transaction.state();
    let mut written = 0;
    for line in &state.lines {
        let source = MovementSource::SaleLine {
            transaction_id: state.transaction_id,
            line_number: line.line_number,
        };
        let source_ref = source.as_ref_string();
        if uow.inventory_movement_exists(
            &device.store_id,
            MovementType::Sale,
            &line.product_ref,
            &source_ref,
        )? {
            continue;
        }
        // The physical quantity, not the billed one: a buy-2-get-1 line is
        // three units leaving the store, and the promotion only touched the
        // money (ADR 027).
        let movement = InventoryMovement::record(NewInventoryMovement {
            movement_id: ids.new_uuid(),
            store_id: device.store_id.clone(),
            product_ref: line.product_ref.clone(),
            movement_type: MovementType::Sale,
            quantity: line.quantity,
            source,
            business_day_id: Some(business_day_id),
            session_id: Some(session_id),
            reason_code: None,
            occurred_at: now,
        })
        .map_err(AppError::Inventory)?;
        uow.insert_inventory_movement(&movement)?;
        written += 1;
    }
    Ok(written)
}

/// Availability for a whole basket, checked before any money moves (ADR 029).
///
/// Returns `Ok(())` when the policy allows negative stock, so a till with a
/// stale position never refuses a sale it was not configured to refuse.
///
/// What a refusal means is bounded: *this installation's projection* says the
/// stock is not here. It is not a statement that the store's shelves are
/// empty, and passing the check is not a guarantee that another register has
/// not already sold the unit while offline (ADR 026). The check is worth
/// having anyway — it catches the single-register case and the obvious
/// oversell — but it is not a distributed invariant.
pub(crate) fn require_available_stock(
    uow: &mut dyn UnitOfWork,
    device: &DeviceProfile,
    transaction: &RetailTransaction,
) -> Result<(), AppError> {
    if device.allow_negative_inventory {
        return Ok(());
    }
    // One line can appear twice for the same product, so the basket is folded
    // first: two lines of three must not each pass against a stock of four.
    let mut wanted: Vec<(ProductRef, i64)> = Vec::new();
    for line in &transaction.state().lines {
        match wanted
            .iter_mut()
            .find(|(product, _)| product == &line.product_ref)
        {
            Some((_, total)) => *total += line.quantity.thousandths(),
            None => wanted.push((line.product_ref.clone(), line.quantity.thousandths())),
        }
    }
    for (product_ref, requested) in wanted {
        let position = uow.inventory_position(&device.store_id, &product_ref)?;
        if position.on_hand_thousandths < requested {
            return Err(AppError::Inventory(InventoryError::InsufficientStock {
                product_ref: product_ref.to_string(),
                on_hand: position.on_hand_display(),
                requested: render_thousandths(requested),
            }));
        }
    }
    Ok(())
}

/// Minting ids inside a transaction without dragging the whole generic core in.
///
/// `record_sale_movements` is called from a `&mut dyn UnitOfWork` closure that
/// does not know `I`, and a sale can have any number of lines, so ids have to
/// be minted on demand rather than pre-counted.
pub(crate) trait IdFactoryRef {
    fn new_uuid(&self) -> Uuid;
}

impl<T: IdFactory> IdFactoryRef for T {
    fn new_uuid(&self) -> Uuid {
        IdFactory::new_uuid(self)
    }
}

fn render_thousandths(thousandths: i64) -> String {
    let units = thousandths / Quantity::SCALE;
    let fraction = (thousandths % Quantity::SCALE).unsigned_abs();
    format!("{units}.{fraction:03}")
}

/// Inventory does not keep a product list, so the catalog decides what exists.
fn require_known_product(
    uow: &mut dyn UnitOfWork,
    snapshot_id: CatalogSnapshotId,
    product_ref: &ProductRef,
) -> Result<(), AppError> {
    let found = uow.lookup_catalog_product(
        snapshot_id,
        &edge_catalog::ProductSelector::ProductRef(product_ref.clone()),
    )?;
    if found.is_none() {
        return Err(AppError::Inventory(InventoryError::UnknownProduct {
            product_ref: product_ref.to_string(),
        }));
    }
    Ok(())
}

/// Recompute a position after a fold, for callers that already have the
/// movements in hand. Keeps the domain's arithmetic as the single definition.
#[allow(dead_code)]
pub(crate) fn position_from(
    store_id: StoreId,
    product_ref: ProductRef,
    movements: &[InventoryMovement],
) -> InventoryPosition {
    on_hand(store_id, product_ref, movements)
}
