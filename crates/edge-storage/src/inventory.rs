//! SQLite adapter for the inventory context.
//!
//! The ledger is append-only, so there is no `UPDATE` or `DELETE` statement
//! against `inventory_movements` anywhere in this file — and triggers in
//! migration 0007 make it impossible rather than merely unimplemented
//! (ADR 026).
//!
//! `on_hand` is folded by the database rather than in Rust: a long ledger
//! should not travel across the boundary just to be summed. The arithmetic is
//! the same `Σ(IN) − Σ(OUT)` the domain defines, and a unit test in
//! `edge-inventory` pins it.

use edge_application::ports::{InventoryBootstrapState, InventoryUnitOfWork};
use edge_application::AppError;
use edge_domain::prelude::*;
use edge_inventory::{
    Direction, InventoryError, InventoryMovement, InventoryPosition, MovementType,
};
use rusqlite::{params, OptionalExtension, Row};
use uuid::Uuid;

use crate::error::sqlite_err;
use crate::sqlite::{parse_id, parse_opt_ts, parse_ts, SqliteUnitOfWork};

const MOVEMENT_COLUMNS: &str = "movement_id, store_id, product_ref, movement_type, direction, \
     quantity_thousandths, source_ref, transaction_id, line_number, business_day_id, \
     session_id, reason_code, occurred_at";

impl InventoryUnitOfWork for SqliteUnitOfWork<'_, '_> {
    fn insert_inventory_movement(&mut self, movement: &InventoryMovement) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO inventory_movements
                     (movement_id, store_id, product_ref, movement_type, direction,
                      quantity_thousandths, source_ref, transaction_id, line_number,
                      business_day_id, session_id, reason_code, occurred_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    movement.movement_id.to_string(),
                    movement.store_id.as_str(),
                    movement.product_ref.as_str(),
                    movement.movement_type.as_str(),
                    movement.direction.as_str(),
                    movement.quantity.thousandths(),
                    movement.source_ref,
                    movement.transaction_id.map(|id| id.to_string()),
                    movement.line_number,
                    movement.business_day_id.map(|id| id.to_string()),
                    movement.session_id.map(|id| id.to_string()),
                    movement.reason_code,
                    movement.occurred_at.to_rfc3339(),
                ],
            )
            .map_err(|error| {
                if is_constraint_violation(&error) {
                    // The unique index on (store, type, source_ref) is what
                    // makes a replayed finalization unable to move stock twice.
                    AppError::Inventory(InventoryError::InvalidRecord {
                        field: "source_ref",
                        reason: "this cause has already moved this product".to_owned(),
                    })
                } else {
                    sqlite_err(error)
                }
            })?;
        Ok(())
    }

    fn list_inventory_movements(
        &mut self,
        store_id: &StoreId,
        product_ref: &ProductRef,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<InventoryMovement>, AppError> {
        let mut statement = self
            .tx
            .prepare(&format!(
                "SELECT {MOVEMENT_COLUMNS} FROM inventory_movements
                 WHERE store_id = ?1 AND product_ref = ?2
                 ORDER BY occurred_at ASC, movement_id ASC
                 LIMIT ?3 OFFSET ?4"
            ))
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(
                params![store_id.as_str(), product_ref.as_str(), limit, offset],
                RawMovement::from_row,
            )
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawMovement::into_domain).collect()
    }

    fn inventory_position(
        &mut self,
        store_id: &StoreId,
        product_ref: &ProductRef,
    ) -> Result<InventoryPosition, AppError> {
        let (on_hand, count, last): (Option<i64>, i64, Option<String>) = self
            .tx
            .query_row(
                "SELECT
                     SUM(CASE direction WHEN 'IN' THEN quantity_thousandths
                                        ELSE -quantity_thousandths END),
                     COUNT(*),
                     MAX(occurred_at)
                 FROM inventory_movements
                 WHERE store_id = ?1 AND product_ref = ?2",
                params![store_id.as_str(), product_ref.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(sqlite_err)?;
        Ok(InventoryPosition {
            store_id: store_id.clone(),
            product_ref: product_ref.clone(),
            on_hand_thousandths: on_hand.unwrap_or(0),
            movement_count: count,
            last_movement_at: parse_opt_ts(last)?,
        })
    }

    fn inventory_movement_exists(
        &mut self,
        store_id: &StoreId,
        movement_type: MovementType,
        product_ref: &ProductRef,
        source_ref: &str,
    ) -> Result<bool, AppError> {
        let found: Option<i64> = self
            .tx
            .query_row(
                "SELECT 1 FROM inventory_movements
                 WHERE store_id = ?1 AND movement_type = ?2 AND product_ref = ?3
                   AND source_ref = ?4",
                params![
                    store_id.as_str(),
                    movement_type.as_str(),
                    product_ref.as_str(),
                    source_ref
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_err)?;
        Ok(found.is_some())
    }

    fn find_inventory_bootstrap(
        &mut self,
        store_id: &StoreId,
    ) -> Result<Option<InventoryBootstrapState>, AppError> {
        self.tx
            .query_row(
                "SELECT store_id, source_snapshot_id, generated_at, item_count, bootstrapped_at
                 FROM inventory_bootstrap_state WHERE store_id = ?1",
                params![store_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(sqlite_err)?
            .map(|(store, snapshot, generated, count, at)| {
                Ok(InventoryBootstrapState {
                    store_id: parse_id(&store)?,
                    source_snapshot_id: snapshot,
                    generated_at: parse_ts(&generated)?,
                    item_count: count,
                    bootstrapped_at: parse_ts(&at)?,
                })
            })
            .transpose()
    }

    fn record_inventory_bootstrap(
        &mut self,
        state: &InventoryBootstrapState,
    ) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO inventory_bootstrap_state
                     (store_id, source_snapshot_id, generated_at, item_count, bootstrapped_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    state.store_id.as_str(),
                    state.source_snapshot_id,
                    state.generated_at.to_rfc3339(),
                    state.item_count,
                    state.bootstrapped_at.to_rfc3339(),
                ],
            )
            .map_err(|error| {
                if is_constraint_violation(&error) {
                    AppError::Inventory(InventoryError::AlreadyBootstrapped {
                        source_snapshot_id: state.source_snapshot_id.clone(),
                    })
                } else {
                    sqlite_err(error)
                }
            })?;
        Ok(())
    }
}

fn is_constraint_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

// ------------------------------------------------------------ row conversions

struct RawMovement {
    movement_id: String,
    store_id: String,
    product_ref: String,
    movement_type: String,
    direction: String,
    quantity_thousandths: i64,
    source_ref: String,
    transaction_id: Option<String>,
    line_number: Option<i64>,
    business_day_id: Option<String>,
    session_id: Option<String>,
    reason_code: Option<String>,
    occurred_at: String,
}

impl RawMovement {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            movement_id: row.get(0)?,
            store_id: row.get(1)?,
            product_ref: row.get(2)?,
            movement_type: row.get(3)?,
            direction: row.get(4)?,
            quantity_thousandths: row.get(5)?,
            source_ref: row.get(6)?,
            transaction_id: row.get(7)?,
            line_number: row.get(8)?,
            business_day_id: row.get(9)?,
            session_id: row.get(10)?,
            reason_code: row.get(11)?,
            occurred_at: row.get(12)?,
        })
    }

    fn into_domain(self) -> Result<InventoryMovement, AppError> {
        Ok(InventoryMovement::rehydrate(InventoryMovement {
            movement_id: Uuid::parse_str(&self.movement_id).map_err(|_| {
                AppError::Inventory(InventoryError::InvalidRecord {
                    field: "movement_id",
                    reason: format!("`{}` is not a UUID", self.movement_id),
                })
            })?,
            store_id: parse_id(&self.store_id)?,
            product_ref: parse_id(&self.product_ref)?,
            movement_type: MovementType::parse(&self.movement_type).map_err(AppError::Inventory)?,
            direction: Direction::parse(&self.direction).map_err(AppError::Inventory)?,
            quantity: Quantity::from_thousandths(self.quantity_thousandths)
                .map_err(|error| AppError::Inventory(InventoryError::Quantity(error)))?,
            source_ref: self.source_ref,
            transaction_id: self.transaction_id.as_deref().map(parse_id).transpose()?,
            line_number: self.line_number,
            business_day_id: self.business_day_id.as_deref().map(parse_id).transpose()?,
            session_id: self.session_id.as_deref().map(parse_id).transpose()?,
            reason_code: self.reason_code,
            occurred_at: parse_ts(&self.occurred_at)?,
        }))
    }
}
