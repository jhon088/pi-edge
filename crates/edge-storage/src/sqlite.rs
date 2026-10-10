//! The SQLite adapter: `EdgeStore` + `UnitOfWork` implementations.

use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Duration;

use edge_application::ports::{DeviceProfile, EdgeStore, PaymentUnitOfWork, UnitOfWork};
use edge_application::AppError;
use edge_domain::prelude::*;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use serde_json::Value;
use uuid::Uuid;

use crate::error::{sqlite_err, StorageError};
use crate::migrations;

/// How long a writer waits for a competing lock before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Durable store for one edge device.
///
/// A POS terminal has exactly one writer, so a single connection behind a
/// mutex is both sufficient and the simplest way to guarantee that a unit of
/// work is serialized end to end.
#[derive(Debug)]
pub struct SqliteEdgeStore {
    conn: Mutex<Connection>,
    /// Commits still to survive before one is aborted. `0` disarms.
    #[cfg(feature = "fault-injection")]
    fail_before_commit: std::sync::atomic::AtomicI64,
}

impl SqliteEdgeStore {
    /// Open (creating if needed) the local database and bring it to the
    /// current schema version.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| StorageError::Io {
                    path: parent.display().to_string(),
                    source,
                })?;
            }
        }
        let conn = Connection::open(path)?;
        Self::bootstrap(conn, path.display().to_string())
    }

    /// In-memory store, for tests that do not need to survive a restart.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory()?;
        Self::bootstrap(conn, ":memory:".to_owned())
    }

    fn bootstrap(conn: Connection, label: String) -> Result<Self, StorageError> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // WAL returns the resulting mode as a row, hence query_row.
        let journal_mode: String =
            conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrations::apply(&conn)?;
        tracing::info!(
            database = %label,
            journal_mode = %journal_mode,
            schema_version = migrations::target_version(),
            "edge store ready"
        );
        Ok(Self {
            conn: Mutex::new(conn),
            #[cfg(feature = "fault-injection")]
            fail_before_commit: std::sync::atomic::AtomicI64::new(0),
        })
    }

    /// Test hook: abort the next unit of work after the writes but **before**
    /// `COMMIT`, simulating a power loss at the worst possible moment.
    #[cfg(feature = "fault-injection")]
    pub fn arm_commit_failure(&self) {
        self.arm_commit_failure_after(0);
    }

    /// Test hook: let `survive` commits through, then abort the next one.
    ///
    /// Needed by use cases that deliberately span several transactions — a
    /// sync cycle claims in one and records the answer in another — so a test
    /// can say *which* commit dies rather than only ever killing the first.
    #[cfg(feature = "fault-injection")]
    pub fn arm_commit_failure_after(&self, survive: u32) {
        self.fail_before_commit
            .store(i64::from(survive) + 1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl EdgeStore for SqliteEdgeStore {
    fn transact<T, F>(&self, work: F) -> Result<T, AppError>
    where
        F: FnOnce(&mut dyn UnitOfWork) -> Result<T, AppError>,
    {
        let mut guard = self
            .conn
            .lock()
            .map_err(|_| AppError::Storage("edge store mutex poisoned".to_owned()))?;
        let tx = guard
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_err)?;

        // On `Err` the transaction is dropped, which rolls back every write.
        let outcome = {
            let mut uow = SqliteUnitOfWork { tx: &tx };
            work(&mut uow)?
        };

        #[cfg(feature = "fault-injection")]
        if self
            .fail_before_commit
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0
            && self
                .fail_before_commit
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst)
                == 1
        {
            drop(tx);
            return Err(AppError::Storage(
                "injected failure before commit".to_owned(),
            ));
        }

        tx.commit().map_err(sqlite_err)?;
        Ok(outcome)
    }
}

pub(crate) struct SqliteUnitOfWork<'a, 'conn> {
    pub(crate) tx: &'a Transaction<'conn>,
}

impl UnitOfWork for SqliteUnitOfWork<'_, '_> {
    fn device_profile(&mut self) -> Result<Option<DeviceProfile>, AppError> {
        let row = self
            .tx
            .query_row(
                "SELECT tenant_id, store_id, currency, allow_negative_inventory
                 FROM device_profile WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(sqlite_err)?;
        match row {
            None => Ok(None),
            Some((tenant_id, store_id, currency, allow_negative)) => Ok(Some(DeviceProfile {
                tenant_id: parse_id(&tenant_id)?,
                store_id: parse_id(&store_id)?,
                currency: parse_currency(&currency)?,
                allow_negative_inventory: allow_negative != 0,
            })),
        }
    }

    fn put_device_profile(
        &mut self,
        profile: &DeviceProfile,
        now: Timestamp,
    ) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO device_profile
                     (id, tenant_id, store_id, currency, allow_negative_inventory,
                      created_at, updated_at)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                     tenant_id = excluded.tenant_id,
                     store_id = excluded.store_id,
                     currency = excluded.currency,
                     allow_negative_inventory = excluded.allow_negative_inventory,
                     updated_at = excluded.updated_at",
                params![
                    profile.tenant_id.as_str(),
                    profile.store_id.as_str(),
                    profile.currency.code(),
                    i64::from(profile.allow_negative_inventory),
                    now.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn find_open_business_day(
        &mut self,
        store_id: &StoreId,
    ) -> Result<Option<BusinessDay>, AppError> {
        let raw = self
            .tx
            .query_row(
                "SELECT business_day_id, tenant_id, store_id, status, opened_at, closed_at, version,
                        closure_kind, close_reason, closed_by
                 FROM business_days
                 WHERE store_id = ?1 AND status <> 'CLOSED'
                 ORDER BY opened_at DESC
                 LIMIT 1",
                params![store_id.as_str()],
                RawBusinessDay::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        raw.map(RawBusinessDay::into_domain).transpose()
    }

    fn find_business_day(&mut self, id: BusinessDayId) -> Result<Option<BusinessDay>, AppError> {
        let raw = self
            .tx
            .query_row(
                "SELECT business_day_id, tenant_id, store_id, status, opened_at, closed_at, version,
                        closure_kind, close_reason, closed_by
                 FROM business_days WHERE business_day_id = ?1",
                params![id.to_string()],
                RawBusinessDay::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        raw.map(RawBusinessDay::into_domain).transpose()
    }

    fn insert_business_day(&mut self, day: &BusinessDay, now: Timestamp) -> Result<(), AppError> {
        let state = day.state();
        self.tx
            .execute(
                "INSERT INTO business_days
                     (business_day_id, tenant_id, store_id, status, opened_at, closed_at,
                      version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![
                    state.business_day_id.to_string(),
                    state.tenant_id.as_str(),
                    state.store_id.as_str(),
                    state.status.as_str(),
                    state.opened_at.to_rfc3339(),
                    state.closed_at.map(|ts| ts.to_rfc3339()),
                    state.version,
                    now.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn update_business_day(&mut self, day: &BusinessDay, now: Timestamp) -> Result<(), AppError> {
        let state = day.state();
        let affected = self
            .tx
            .execute(
                "UPDATE business_days
                 SET status = ?2, closed_at = ?3, version = ?4, updated_at = ?5,
                     closure_kind = ?6, close_reason = ?7, closed_by = ?8
                 WHERE business_day_id = ?1 AND version < ?4",
                params![
                    state.business_day_id.to_string(),
                    state.status.as_str(),
                    state.closed_at.map(|ts| ts.to_rfc3339()),
                    state.version,
                    now.to_rfc3339(),
                    state.closure_kind.map(ClosureKind::as_str),
                    state.close_reason,
                    state.closed_by,
                ],
            )
            .map_err(sqlite_err)?;
        if affected != 1 {
            return Err(AppError::ConcurrentModification {
                aggregate: "BusinessDay",
                id: state.business_day_id.to_string(),
            });
        }
        Ok(())
    }

    fn find_open_sessions(
        &mut self,
        business_day_id: BusinessDayId,
    ) -> Result<Vec<RegisterSession>, AppError> {
        let mut stmt = self
            .tx
            .prepare(
                "SELECT session_id, business_day_id, register_id, operator_id, status,
                        opening_cash_minor, currency, opened_at, closed_at, version,
                        closure_kind, close_declaration_id, reopened_from_session_id
                 FROM register_sessions
                 WHERE business_day_id = ?1 AND status <> 'CLOSED'
                 ORDER BY opened_at ASC",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![business_day_id.to_string()], RawSession::from_row)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawSession::into_domain).collect()
    }

    fn find_open_session_for_register(
        &mut self,
        register_id: &RegisterId,
    ) -> Result<Option<RegisterSession>, AppError> {
        let raw = self
            .tx
            .query_row(
                "SELECT session_id, business_day_id, register_id, operator_id, status,
                        opening_cash_minor, currency, opened_at, closed_at, version,
                        closure_kind, close_declaration_id, reopened_from_session_id
                 FROM register_sessions
                 WHERE register_id = ?1 AND status <> 'CLOSED'
                 LIMIT 1",
                params![register_id.as_str()],
                RawSession::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        raw.map(RawSession::into_domain).transpose()
    }

    fn insert_register_session(
        &mut self,
        session: &RegisterSession,
        now: Timestamp,
    ) -> Result<(), AppError> {
        let state = session.state();
        self.tx
            .execute(
                "INSERT INTO register_sessions
                     (session_id, business_day_id, register_id, operator_id, status,
                      opening_cash_minor, currency, opened_at, closed_at, version,
                      reopened_from_session_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
                params![
                    state.session_id.to_string(),
                    state.business_day_id.to_string(),
                    state.register_id.as_str(),
                    state.operator_id.as_str(),
                    state.status.as_str(),
                    state.opening_cash.minor(),
                    state.opening_cash.currency().code(),
                    state.opened_at.to_rfc3339(),
                    state.closed_at.map(|ts| ts.to_rfc3339()),
                    state.version,
                    state.reopened_from_session_id.map(|id| id.to_string()),
                    now.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn update_register_session(
        &mut self,
        session: &RegisterSession,
        now: Timestamp,
    ) -> Result<(), AppError> {
        let state = session.state();
        let affected = self
            .tx
            .execute(
                "UPDATE register_sessions
                 SET status = ?2, closed_at = ?3, version = ?4, updated_at = ?5,
                     closure_kind = ?6, close_declaration_id = ?7
                 WHERE session_id = ?1 AND version < ?4",
                params![
                    state.session_id.to_string(),
                    state.status.as_str(),
                    state.closed_at.map(|ts| ts.to_rfc3339()),
                    state.version,
                    now.to_rfc3339(),
                    state.closure_kind.map(ClosureKind::as_str),
                    state.close_declaration_id.map(|id| id.to_string()),
                ],
            )
            .map_err(sqlite_err)?;
        if affected != 1 {
            return Err(AppError::ConcurrentModification {
                aggregate: "RegisterSession",
                id: state.session_id.to_string(),
            });
        }
        Ok(())
    }

    fn find_active_transaction(
        &mut self,
        session_id: SessionId,
    ) -> Result<Option<RetailTransaction>, AppError> {
        let raw = self
            .tx
            .query_row(
                "SELECT transaction_id, business_day_id, session_id, store_id, register_id,
                        operator_id, register_sequence, status, currency, subtotal_minor,
                        discount_total_minor, net_total_minor, tax_total_minor,
                        grand_total_minor, created_at, updated_at, finalized_at, version,
                        catalog_snapshot_id, catalog_version, pricing_effective_at,
                        next_line_number
                 FROM retail_transactions
                 WHERE session_id = ?1 AND status NOT IN ('FINALIZED', 'CANCELLED')
                 ORDER BY register_sequence DESC
                 LIMIT 1",
                params![session_id.to_string()],
                RawTransaction::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        match raw {
            None => Ok(None),
            Some(raw) => Ok(Some(self.load_transaction(raw)?)),
        }
    }

    fn find_transaction(
        &mut self,
        id: TransactionId,
    ) -> Result<Option<RetailTransaction>, AppError> {
        let raw = self
            .tx
            .query_row(
                "SELECT transaction_id, business_day_id, session_id, store_id, register_id,
                        operator_id, register_sequence, status, currency, subtotal_minor,
                        discount_total_minor, net_total_minor, tax_total_minor,
                        grand_total_minor, created_at, updated_at, finalized_at, version,
                        catalog_snapshot_id, catalog_version, pricing_effective_at,
                        next_line_number
                 FROM retail_transactions
                 WHERE transaction_id = ?1",
                params![id.to_string()],
                RawTransaction::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        match raw {
            None => Ok(None),
            Some(raw) => Ok(Some(self.load_transaction(raw)?)),
        }
    }

    fn next_register_sequence(&mut self, register_id: &RegisterId) -> Result<i64, AppError> {
        self.tx
            .query_row(
                "SELECT COALESCE(MAX(register_sequence), 0) + 1
                 FROM retail_transactions WHERE register_id = ?1",
                params![register_id.as_str()],
                |row| row.get(0),
            )
            .map_err(sqlite_err)
    }

    fn insert_transaction(
        &mut self,
        transaction: &RetailTransaction,
        now: Timestamp,
    ) -> Result<(), AppError> {
        let state = transaction.state();
        self.tx
            .execute(
                "INSERT INTO retail_transactions
                     (transaction_id, business_day_id, session_id, store_id, register_id,
                      operator_id, register_sequence, status, currency, subtotal_minor,
                      discount_total_minor, net_total_minor, tax_total_minor, grand_total_minor,
                      created_at, updated_at, finalized_at, version,
                      catalog_snapshot_id, catalog_version, pricing_effective_at,
                      next_line_number)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?20, ?21, ?22)",
                params![
                    state.transaction_id.to_string(),
                    state.business_day_id.to_string(),
                    state.session_id.to_string(),
                    state.store_id.as_str(),
                    state.register_id.as_str(),
                    state.operator_id.as_str(),
                    state.register_sequence,
                    state.status.as_str(),
                    state.currency.code(),
                    state.totals.subtotal.minor(),
                    state.totals.discount_total.minor(),
                    state.totals.net_total.minor(),
                    state.totals.tax_total.minor(),
                    state.totals.grand_total.minor(),
                    state.created_at.to_rfc3339(),
                    now.to_rfc3339(),
                    state.finalized_at.map(|ts| ts.to_rfc3339()),
                    state.version,
                    state.pricing.map(|pricing| pricing.snapshot_id.to_string()),
                    state.pricing.map(|pricing| pricing.catalog_version),
                    state
                        .pricing
                        .map(|pricing| pricing.effective_at.to_rfc3339()),
                    state.next_line_number,
                ],
            )
            .map_err(sqlite_err)?;
        self.write_children(transaction, now)
    }

    fn update_transaction(
        &mut self,
        transaction: &RetailTransaction,
        now: Timestamp,
    ) -> Result<(), AppError> {
        let state = transaction.state();
        let affected = self
            .tx
            .execute(
                "UPDATE retail_transactions
                 SET status = ?2, subtotal_minor = ?3, discount_total_minor = ?4,
                     net_total_minor = ?5, tax_total_minor = ?6, grand_total_minor = ?7,
                     updated_at = ?8, finalized_at = ?9, version = ?10,
                     next_line_number = ?11
                 WHERE transaction_id = ?1 AND version < ?10",
                params![
                    state.transaction_id.to_string(),
                    state.status.as_str(),
                    state.totals.subtotal.minor(),
                    state.totals.discount_total.minor(),
                    state.totals.net_total.minor(),
                    state.totals.tax_total.minor(),
                    state.totals.grand_total.minor(),
                    now.to_rfc3339(),
                    state.finalized_at.map(|ts| ts.to_rfc3339()),
                    state.version,
                    state.next_line_number,
                ],
            )
            .map_err(sqlite_err)?;
        if affected != 1 {
            return Err(AppError::ConcurrentModification {
                aggregate: "RetailTransaction",
                id: state.transaction_id.to_string(),
            });
        }
        self.write_children(transaction, now)
    }

    fn append_journal(&mut self, event: &JournalEvent) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO journal_events
                     (event_id, event_type, aggregate_id, tenant_id, store_id, register_id,
                      business_day_id, session_id, transaction_id, operator_id, occurred_at,
                      payload_json, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?11)",
                params![
                    event.event_id.to_string(),
                    event.event_type.as_str(),
                    event.aggregate_id,
                    event.tenant_id.as_str(),
                    event.store_id.as_str(),
                    event.register_id.as_ref().map(RegisterId::as_str),
                    event.business_day_id.map(|id| id.to_string()),
                    event.session_id.map(|id| id.to_string()),
                    event.transaction_id.map(|id| id.to_string()),
                    event.operator_id.as_ref().map(OperatorId::as_str),
                    event.occurred_at.to_rfc3339(),
                    to_json(&event.payload_json)?,
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn list_journal(&mut self, limit: i64) -> Result<Vec<JournalEvent>, AppError> {
        let mut stmt = self
            .tx
            .prepare(
                "SELECT event_id, event_type, aggregate_id, tenant_id, store_id, register_id,
                        business_day_id, session_id, transaction_id, operator_id, occurred_at,
                        payload_json
                 FROM journal_events
                 ORDER BY seq DESC
                 LIMIT ?1",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![limit], RawJournalEvent::from_row)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawJournalEvent::into_domain).collect()
    }

    fn enqueue_outbox(&mut self, event: &OutboxEvent) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO outbox_events
                     (event_id, event_type, aggregate_id, aggregate_version, tenant_id, store_id,
                      register_id, business_day_id, occurred_at, payload_json, status,
                      attempt_count, next_attempt_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?9, ?9)",
                params![
                    event.event_id.to_string(),
                    event.event_type.as_str(),
                    event.aggregate_id,
                    event.aggregate_version,
                    event.tenant_id.as_str(),
                    event.store_id.as_str(),
                    event.register_id.as_ref().map(RegisterId::as_str),
                    event.business_day_id.map(|id| id.to_string()),
                    event.occurred_at.to_rfc3339(),
                    to_json(&event.payload_json)?,
                    event.status.as_str(),
                    event.attempt_count,
                    event.next_attempt_at.map(|ts| ts.to_rfc3339()),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    /// Everything not yet confirmed as delivered (`PENDING` + `SENDING`).
    fn count_pending_outbox(&mut self) -> Result<i64, AppError> {
        self.tx
            .query_row(
                "SELECT COUNT(*) FROM outbox_events WHERE status <> 'SENT'",
                [],
                |row| row.get(0),
            )
            .map_err(sqlite_err)
    }

    fn list_pending_outbox(&mut self, limit: i64) -> Result<Vec<OutboxEvent>, AppError> {
        let mut stmt = self
            .tx
            .prepare(
                "SELECT event_id, event_type, aggregate_id, aggregate_version, tenant_id,
                        store_id, register_id, business_day_id, occurred_at, payload_json,
                        status, attempt_count, next_attempt_at
                 FROM outbox_events
                 WHERE status <> 'SENT'
                 ORDER BY seq ASC
                 LIMIT ?1",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![limit], RawOutboxEvent::from_row)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawOutboxEvent::into_domain).collect()
    }

    fn find_command_result(&mut self, request_id: Uuid) -> Result<Option<String>, AppError> {
        self.tx
            .query_row(
                "SELECT result_json FROM processed_commands WHERE request_id = ?1",
                params![request_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_err)
    }

    fn record_command_result(
        &mut self,
        request_id: Uuid,
        command_type: &str,
        result_json: &str,
        now: Timestamp,
    ) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO processed_commands (request_id, command_type, result_json, processed_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    request_id.to_string(),
                    command_type,
                    result_json,
                    now.to_rfc3339()
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }
}

impl SqliteUnitOfWork<'_, '_> {
    /// A line is no longer write-once: it can be removed, its quantity can
    /// change, and a promotion can put a discount on it. So the stored set is
    /// reconciled with the in-memory one — rows that are gone are deleted, and
    /// the mutable columns of the rest are refreshed. `created_at` and the
    /// line's identity are never touched. Payments can only gain their commit
    /// timestamp, so they stay a plain upsert.
    fn write_children(
        &self,
        transaction: &RetailTransaction,
        now: Timestamp,
    ) -> Result<(), AppError> {
        let state = transaction.state();
        self.prune_children(
            "transaction_lines",
            "line_id",
            &state.transaction_id.to_string(),
            &state
                .lines
                .iter()
                .map(|line| line.line_id.to_string())
                .collect::<Vec<_>>(),
        )?;
        for line in &state.lines {
            self.tx
                .execute(
                    "INSERT INTO transaction_lines
                         (line_id, transaction_id, line_number, product_ref, sku, barcode,
                          description, quantity_thousandths, unit_price_minor, tax_rate_bps,
                          price_mode, price_book_id, extended_amount_minor,
                          discount_amount_minor, taxable_amount_minor, tax_amount_minor,
                          line_total_minor, currency, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                             ?16, ?17, ?18, ?19)
                     ON CONFLICT(line_id) DO UPDATE SET
                         quantity_thousandths = excluded.quantity_thousandths,
                         extended_amount_minor = excluded.extended_amount_minor,
                         discount_amount_minor = excluded.discount_amount_minor,
                         taxable_amount_minor = excluded.taxable_amount_minor,
                         tax_amount_minor = excluded.tax_amount_minor,
                         line_total_minor = excluded.line_total_minor",
                    params![
                        line.line_id.to_string(),
                        state.transaction_id.to_string(),
                        line.line_number,
                        line.product_ref.as_str(),
                        line.sku,
                        line.barcode,
                        line.description,
                        line.quantity.thousandths(),
                        line.unit_price.minor(),
                        line.tax_rate.bps(),
                        line.price_mode.as_str(),
                        line.price_book_id.as_ref().map(PriceBookId::as_str),
                        line.extended_amount.minor(),
                        line.discount_amount.minor(),
                        line.taxable_amount.minor(),
                        line.tax_amount.minor(),
                        line.line_total.minor(),
                        line.unit_price.currency().code(),
                        now.to_rfc3339(),
                    ],
                )
                .map_err(sqlite_err)?;
        }
        // Payments are not children of the sale: they belong to the payment
        // context and are written by its own adapter (ADR 016). The sale only
        // carries a derived summary, which is recomputed on load.
        self.write_promotions(state, now)
    }

    /// Promotion records are keyed by their deterministic adjustment id, so
    /// re-evaluating the same basket rewrites the same rows. An adjustment that
    /// stops applying has to disappear, hence the prune.
    fn write_promotions(
        &self,
        state: &RetailTransactionState,
        now: Timestamp,
    ) -> Result<(), AppError> {
        let transaction_id = state.transaction_id.to_string();
        self.prune_children(
            "transaction_promotions",
            "adjustment_id",
            &transaction_id,
            &state
                .applied_promotions
                .iter()
                .map(|applied| applied.adjustment_id.clone())
                .collect::<Vec<_>>(),
        )?;
        for (sequence, applied) in state.applied_promotions.iter().enumerate() {
            self.tx
                .execute(
                    "INSERT INTO transaction_promotions
                         (transaction_id, adjustment_id, sequence, promotion_id, promotion_code,
                          promotion_name, priority, stackability, target, line_number,
                          amount_minor, currency, reason_code, detail, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                     ON CONFLICT(transaction_id, adjustment_id) DO UPDATE SET
                         sequence = excluded.sequence,
                         amount_minor = excluded.amount_minor,
                         detail = excluded.detail",
                    params![
                        transaction_id,
                        applied.adjustment_id,
                        i64::try_from(sequence).unwrap_or(i64::MAX),
                        applied.promotion_id,
                        applied.code,
                        applied.name,
                        applied.priority,
                        applied.stackability,
                        applied.target.kind(),
                        applied.target.line_number(),
                        applied.amount.minor(),
                        applied.amount.currency().code(),
                        applied.reason,
                        applied.detail,
                        now.to_rfc3339(),
                    ],
                )
                .map_err(sqlite_err)?;
        }
        Ok(())
    }

    /// Delete the rows of one child table that the aggregate no longer holds.
    ///
    /// The id list is bound as parameters rather than interpolated; the table
    /// and key names are `&'static` call-site literals, never input.
    fn prune_children(
        &self,
        table: &'static str,
        key: &'static str,
        transaction_id: &str,
        keep: &[String],
    ) -> Result<(), AppError> {
        let mut sql = format!("DELETE FROM {table} WHERE transaction_id = ?1");
        if !keep.is_empty() {
            let placeholders: Vec<String> =
                (0..keep.len()).map(|i| format!("?{}", i + 2)).collect();
            sql.push_str(&format!(" AND {key} NOT IN ({})", placeholders.join(", ")));
        }
        let mut bound: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(keep.len() + 1);
        bound.push(&transaction_id);
        for id in keep {
            bound.push(id);
        }
        self.tx
            .execute(&sql, bound.as_slice())
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn load_promotions(&self, transaction_id: &str) -> Result<Vec<AppliedPromotion>, AppError> {
        let mut stmt = self
            .tx
            .prepare(
                "SELECT adjustment_id, promotion_id, promotion_code, promotion_name, priority,
                        stackability, target, line_number, amount_minor, currency, reason_code,
                        detail
                 FROM transaction_promotions
                 WHERE transaction_id = ?1
                 ORDER BY sequence ASC, adjustment_id ASC",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![transaction_id], RawPromotion::from_row)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawPromotion::into_domain).collect()
    }

    fn load_transaction(&mut self, raw: RawTransaction) -> Result<RetailTransaction, AppError> {
        let transaction_id: TransactionId = parse_id(&raw.transaction_id)?;
        let lines = self.load_lines(&raw.transaction_id)?;
        let promotions = self.load_promotions(&raw.transaction_id)?;
        let grand_total = money(raw.grand_total_minor, parse_currency(&raw.currency)?);
        let mut transaction = raw.into_domain(lines, promotions)?;
        // The payment summary is derived, never stored on the sale: the
        // payment tables are the single source of truth (ADR 020).
        let views = self.list_transaction_payments(transaction_id)?;
        let settlement = edge_payments::settle(&views, grand_total).map_err(AppError::Payment)?;
        transaction.restore_payment_summary(settlement.summary());
        Ok(transaction)
    }

    fn load_lines(&self, transaction_id: &str) -> Result<Vec<TransactionLine>, AppError> {
        let mut stmt = self
            .tx
            .prepare(
                "SELECT line_id, line_number, product_ref, sku, barcode, description,
                        quantity_thousandths, unit_price_minor, tax_rate_bps, price_mode,
                        price_book_id, extended_amount_minor, discount_amount_minor,
                        taxable_amount_minor, tax_amount_minor, line_total_minor, currency
                 FROM transaction_lines
                 WHERE transaction_id = ?1
                 ORDER BY line_number ASC",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![transaction_id], RawLine::from_row)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawLine::into_domain).collect()
    }
}

// ------------------------------------------------------------ row conversions

pub(crate) fn parse_id<T>(value: &str) -> Result<T, AppError>
where
    T: FromStr<Err = IdError>,
{
    T::from_str(value).map_err(|e| AppError::Domain(DomainError::Id(e)))
}

pub(crate) fn parse_opt_id<T>(value: Option<String>) -> Result<Option<T>, AppError>
where
    T: FromStr<Err = IdError>,
{
    match value {
        Some(raw) => Ok(Some(parse_id(&raw)?)),
        None => Ok(None),
    }
}

pub(crate) fn parse_ts(value: &str) -> Result<Timestamp, AppError> {
    Timestamp::parse_rfc3339(value).map_err(|e| AppError::Domain(DomainError::Time(e)))
}

pub(crate) fn parse_opt_ts(value: Option<String>) -> Result<Option<Timestamp>, AppError> {
    match value {
        Some(raw) => Ok(Some(parse_ts(&raw)?)),
        None => Ok(None),
    }
}

pub(crate) fn parse_currency(code: &str) -> Result<Currency, AppError> {
    Currency::from_code(code).map_err(|e| AppError::Domain(DomainError::Money(e)))
}

pub(crate) fn parse_json(raw: &str) -> Result<Value, AppError> {
    serde_json::from_str(raw).map_err(|e| AppError::Serialization(e.to_string()))
}

pub(crate) fn to_json(value: &Value) -> Result<String, AppError> {
    serde_json::to_string(value).map_err(|e| AppError::Serialization(e.to_string()))
}

pub(crate) fn money(minor: i64, currency: Currency) -> Money {
    Money::from_minor(minor, currency)
}

struct RawBusinessDay {
    business_day_id: String,
    tenant_id: String,
    store_id: String,
    status: String,
    opened_at: String,
    closed_at: Option<String>,
    version: i64,
    closure_kind: Option<String>,
    close_reason: Option<String>,
    closed_by: Option<String>,
}

impl RawBusinessDay {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            business_day_id: row.get(0)?,
            tenant_id: row.get(1)?,
            store_id: row.get(2)?,
            status: row.get(3)?,
            opened_at: row.get(4)?,
            closed_at: row.get(5)?,
            version: row.get(6)?,
            closure_kind: row.get(7)?,
            close_reason: row.get(8)?,
            closed_by: row.get(9)?,
        })
    }

    fn into_domain(self) -> Result<BusinessDay, AppError> {
        Ok(BusinessDay::rehydrate(BusinessDayState {
            business_day_id: parse_id(&self.business_day_id)?,
            tenant_id: parse_id(&self.tenant_id)?,
            store_id: parse_id(&self.store_id)?,
            status: BusinessDayStatus::parse(&self.status).map_err(AppError::from)?,
            opened_at: parse_ts(&self.opened_at)?,
            closed_at: parse_opt_ts(self.closed_at)?,
            closure_kind: self
                .closure_kind
                .as_deref()
                .map(ClosureKind::parse)
                .transpose()
                .map_err(AppError::from)?,
            close_reason: self.close_reason,
            closed_by: self.closed_by,
            version: self.version,
        }))
    }
}

/// Columns every session query selects. One projection, shared with the cash
/// adapter, so the two can never disagree about a session's shape.
pub(crate) const SESSION_COLUMNS: &str =
    "session_id, business_day_id, register_id, operator_id, status, opening_cash_minor, \
     currency, opened_at, closed_at, version, closure_kind, close_declaration_id, \
     reopened_from_session_id";

pub(crate) struct RawSession {
    session_id: String,
    business_day_id: String,
    register_id: String,
    operator_id: String,
    status: String,
    opening_cash_minor: i64,
    currency: String,
    opened_at: String,
    closed_at: Option<String>,
    version: i64,
    closure_kind: Option<String>,
    close_declaration_id: Option<String>,
    reopened_from_session_id: Option<String>,
}

impl RawSession {
    pub(crate) fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            session_id: row.get(0)?,
            business_day_id: row.get(1)?,
            register_id: row.get(2)?,
            operator_id: row.get(3)?,
            status: row.get(4)?,
            opening_cash_minor: row.get(5)?,
            currency: row.get(6)?,
            opened_at: row.get(7)?,
            closed_at: row.get(8)?,
            version: row.get(9)?,
            closure_kind: row.get(10)?,
            close_declaration_id: row.get(11)?,
            reopened_from_session_id: row.get(12)?,
        })
    }

    pub(crate) fn into_domain(self) -> Result<RegisterSession, AppError> {
        let currency = parse_currency(&self.currency)?;
        Ok(RegisterSession::rehydrate(RegisterSessionState {
            session_id: parse_id(&self.session_id)?,
            business_day_id: parse_id(&self.business_day_id)?,
            register_id: parse_id(&self.register_id)?,
            operator_id: parse_id(&self.operator_id)?,
            status: RegisterSessionStatus::parse(&self.status).map_err(AppError::from)?,
            opening_cash: money(self.opening_cash_minor, currency),
            opened_at: parse_ts(&self.opened_at)?,
            closed_at: parse_opt_ts(self.closed_at)?,
            closure_kind: self
                .closure_kind
                .as_deref()
                .map(ClosureKind::parse)
                .transpose()
                .map_err(AppError::from)?,
            close_declaration_id: self
                .close_declaration_id
                .as_deref()
                .map(parse_uuid_field)
                .transpose()?,
            reopened_from_session_id: self
                .reopened_from_session_id
                .as_deref()
                .map(parse_id)
                .transpose()?,
            version: self.version,
        }))
    }
}

/// A UUID column that is not an opaque domain id.
pub(crate) fn parse_uuid_field(value: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(value).map_err(|_| AppError::Storage(format!("`{value}` is not a UUID")))
}

struct RawTransaction {
    transaction_id: String,
    business_day_id: String,
    session_id: String,
    store_id: String,
    register_id: String,
    operator_id: String,
    register_sequence: i64,
    status: String,
    currency: String,
    subtotal_minor: i64,
    discount_total_minor: i64,
    net_total_minor: i64,
    tax_total_minor: i64,
    grand_total_minor: i64,
    created_at: String,
    updated_at: String,
    finalized_at: Option<String>,
    version: i64,
    catalog_snapshot_id: Option<String>,
    catalog_version: Option<i64>,
    pricing_effective_at: Option<String>,
    next_line_number: i64,
}

impl RawTransaction {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            transaction_id: row.get(0)?,
            business_day_id: row.get(1)?,
            session_id: row.get(2)?,
            store_id: row.get(3)?,
            register_id: row.get(4)?,
            operator_id: row.get(5)?,
            register_sequence: row.get(6)?,
            status: row.get(7)?,
            currency: row.get(8)?,
            subtotal_minor: row.get(9)?,
            discount_total_minor: row.get(10)?,
            net_total_minor: row.get(11)?,
            tax_total_minor: row.get(12)?,
            grand_total_minor: row.get(13)?,
            created_at: row.get(14)?,
            updated_at: row.get(15)?,
            finalized_at: row.get(16)?,
            version: row.get(17)?,
            catalog_snapshot_id: row.get(18)?,
            catalog_version: row.get(19)?,
            pricing_effective_at: row.get(20)?,
            next_line_number: row.get(21)?,
        })
    }

    fn into_domain(
        self,
        lines: Vec<TransactionLine>,
        applied_promotions: Vec<AppliedPromotion>,
    ) -> Result<RetailTransaction, AppError> {
        let currency = parse_currency(&self.currency)?;
        Ok(RetailTransaction::rehydrate(RetailTransactionState {
            transaction_id: parse_id(&self.transaction_id)?,
            business_day_id: parse_id(&self.business_day_id)?,
            session_id: parse_id(&self.session_id)?,
            store_id: parse_id(&self.store_id)?,
            register_id: parse_id(&self.register_id)?,
            operator_id: parse_id(&self.operator_id)?,
            register_sequence: self.register_sequence,
            status: TransactionStatus::parse(&self.status).map_err(AppError::from)?,
            currency,
            pricing: match (
                self.catalog_snapshot_id,
                self.catalog_version,
                self.pricing_effective_at,
            ) {
                (Some(snapshot_id), Some(catalog_version), Some(effective_at)) => {
                    Some(PricingCapture {
                        snapshot_id: parse_id(&snapshot_id)?,
                        catalog_version,
                        effective_at: parse_ts(&effective_at)?,
                    })
                }
                _ => None,
            },
            lines,
            totals: Totals {
                subtotal: money(self.subtotal_minor, currency),
                discount_total: money(self.discount_total_minor, currency),
                net_total: money(self.net_total_minor, currency),
                tax_total: money(self.tax_total_minor, currency),
                grand_total: money(self.grand_total_minor, currency),
            },
            payments: PaymentSummary::unpaid(currency),
            created_at: parse_ts(&self.created_at)?,
            updated_at: parse_ts(&self.updated_at)?,
            finalized_at: parse_opt_ts(self.finalized_at)?,
            version: self.version,
            next_line_number: self.next_line_number,
            applied_promotions,
        }))
    }
}

struct RawPromotion {
    adjustment_id: String,
    promotion_id: String,
    promotion_code: String,
    promotion_name: String,
    priority: i32,
    stackability: String,
    target: String,
    line_number: Option<i64>,
    amount_minor: i64,
    currency: String,
    reason_code: String,
    detail: Option<String>,
}

impl RawPromotion {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            adjustment_id: row.get(0)?,
            promotion_id: row.get(1)?,
            promotion_code: row.get(2)?,
            promotion_name: row.get(3)?,
            priority: row.get(4)?,
            stackability: row.get(5)?,
            target: row.get(6)?,
            line_number: row.get(7)?,
            amount_minor: row.get(8)?,
            currency: row.get(9)?,
            reason_code: row.get(10)?,
            detail: row.get(11)?,
        })
    }

    fn into_domain(self) -> Result<AppliedPromotion, AppError> {
        let currency = parse_currency(&self.currency)?;
        Ok(AppliedPromotion {
            adjustment_id: self.adjustment_id,
            promotion_id: self.promotion_id,
            code: self.promotion_code,
            name: self.promotion_name,
            priority: self.priority,
            stackability: self.stackability,
            target: PromotionTarget::rehydrate(&self.target, self.line_number)
                .map_err(AppError::from)?,
            amount: money(self.amount_minor, currency),
            reason: self.reason_code,
            detail: self.detail,
        })
    }
}

struct RawLine {
    line_id: String,
    line_number: i64,
    product_ref: String,
    sku: Option<String>,
    barcode: Option<String>,
    description: String,
    quantity_thousandths: i64,
    unit_price_minor: i64,
    tax_rate_bps: i64,
    price_mode: String,
    price_book_id: Option<String>,
    extended_amount_minor: i64,
    discount_amount_minor: i64,
    taxable_amount_minor: i64,
    tax_amount_minor: i64,
    line_total_minor: i64,
    currency: String,
}

impl RawLine {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            line_id: row.get(0)?,
            line_number: row.get(1)?,
            product_ref: row.get(2)?,
            sku: row.get(3)?,
            barcode: row.get(4)?,
            description: row.get(5)?,
            quantity_thousandths: row.get(6)?,
            unit_price_minor: row.get(7)?,
            tax_rate_bps: row.get(8)?,
            price_mode: row.get(9)?,
            price_book_id: row.get(10)?,
            extended_amount_minor: row.get(11)?,
            discount_amount_minor: row.get(12)?,
            taxable_amount_minor: row.get(13)?,
            tax_amount_minor: row.get(14)?,
            line_total_minor: row.get(15)?,
            currency: row.get(16)?,
        })
    }

    fn into_domain(self) -> Result<TransactionLine, AppError> {
        let currency = parse_currency(&self.currency)?;
        let tax_rate_bps = u32::try_from(self.tax_rate_bps).map_err(|_| {
            AppError::Domain(DomainError::Money(MoneyError::InvalidTaxRate(u32::MAX)))
        })?;
        Ok(TransactionLine {
            line_id: parse_id(&self.line_id)?,
            line_number: self.line_number,
            product_ref: parse_id(&self.product_ref)?,
            sku: self.sku,
            barcode: self.barcode,
            description: self.description,
            quantity: Quantity::from_thousandths(self.quantity_thousandths)
                .map_err(|e| AppError::Domain(DomainError::Money(e)))?,
            unit_price: money(self.unit_price_minor, currency),
            tax_rate: TaxRate::from_bps(tax_rate_bps)
                .map_err(|e| AppError::Domain(DomainError::Money(e)))?,
            price_mode: PriceMode::parse(&self.price_mode)
                .map_err(|e| AppError::Domain(DomainError::Money(e)))?,
            price_book_id: parse_opt_id(self.price_book_id)?,
            extended_amount: money(self.extended_amount_minor, currency),
            discount_amount: money(self.discount_amount_minor, currency),
            taxable_amount: money(self.taxable_amount_minor, currency),
            tax_amount: money(self.tax_amount_minor, currency),
            line_total: money(self.line_total_minor, currency),
        })
    }
}

struct RawJournalEvent {
    event_id: String,
    event_type: String,
    aggregate_id: String,
    tenant_id: String,
    store_id: String,
    register_id: Option<String>,
    business_day_id: Option<String>,
    session_id: Option<String>,
    transaction_id: Option<String>,
    operator_id: Option<String>,
    occurred_at: String,
    payload_json: String,
}

impl RawJournalEvent {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            event_id: row.get(0)?,
            event_type: row.get(1)?,
            aggregate_id: row.get(2)?,
            tenant_id: row.get(3)?,
            store_id: row.get(4)?,
            register_id: row.get(5)?,
            business_day_id: row.get(6)?,
            session_id: row.get(7)?,
            transaction_id: row.get(8)?,
            operator_id: row.get(9)?,
            occurred_at: row.get(10)?,
            payload_json: row.get(11)?,
        })
    }

    fn into_domain(self) -> Result<JournalEvent, AppError> {
        Ok(JournalEvent {
            event_id: parse_id(&self.event_id)?,
            event_type: JournalEventType::parse(&self.event_type).map_err(AppError::from)?,
            aggregate_id: self.aggregate_id,
            tenant_id: parse_id(&self.tenant_id)?,
            store_id: parse_id(&self.store_id)?,
            register_id: parse_opt_id(self.register_id)?,
            business_day_id: parse_opt_id(self.business_day_id)?,
            session_id: parse_opt_id(self.session_id)?,
            transaction_id: parse_opt_id(self.transaction_id)?,
            operator_id: parse_opt_id(self.operator_id)?,
            occurred_at: parse_ts(&self.occurred_at)?,
            payload_json: parse_json(&self.payload_json)?,
        })
    }
}

pub(crate) struct RawOutboxEvent {
    event_id: String,
    event_type: String,
    aggregate_id: String,
    aggregate_version: i64,
    tenant_id: String,
    store_id: String,
    register_id: Option<String>,
    business_day_id: Option<String>,
    occurred_at: String,
    payload_json: String,
    status: String,
    attempt_count: i64,
    next_attempt_at: Option<String>,
}

impl RawOutboxEvent {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Self::from_offset_row(row, 0)
    }

    /// The same thirteen columns, read starting at `offset`.
    ///
    /// The sync adapter selects `seq` ahead of them, and one reader for both
    /// callers beats two column lists that have to agree.
    pub(crate) fn from_offset_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            event_id: row.get(offset)?,
            event_type: row.get(offset + 1)?,
            aggregate_id: row.get(offset + 2)?,
            aggregate_version: row.get(offset + 3)?,
            tenant_id: row.get(offset + 4)?,
            store_id: row.get(offset + 5)?,
            register_id: row.get(offset + 6)?,
            business_day_id: row.get(offset + 7)?,
            occurred_at: row.get(offset + 8)?,
            payload_json: row.get(offset + 9)?,
            status: row.get(offset + 10)?,
            attempt_count: row.get(offset + 11)?,
            next_attempt_at: row.get(offset + 12)?,
        })
    }

    pub(crate) fn into_domain(self) -> Result<OutboxEvent, AppError> {
        Ok(OutboxEvent {
            event_id: parse_id(&self.event_id)?,
            event_type: OutboxEventType::parse(&self.event_type).map_err(AppError::from)?,
            aggregate_id: self.aggregate_id,
            aggregate_version: self.aggregate_version,
            tenant_id: parse_id(&self.tenant_id)?,
            store_id: parse_id(&self.store_id)?,
            register_id: parse_opt_id(self.register_id)?,
            business_day_id: parse_opt_id(self.business_day_id)?,
            occurred_at: parse_ts(&self.occurred_at)?,
            payload_json: parse_json(&self.payload_json)?,
            status: OutboxStatus::parse(&self.status).map_err(AppError::from)?,
            attempt_count: self.attempt_count,
            next_attempt_at: parse_opt_ts(self.next_attempt_at)?,
        })
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_in_memory_store_with_pragmas_and_schema() {
        let store = SqliteEdgeStore::open_in_memory().unwrap();
        let guard = store.conn.lock().unwrap();
        let foreign_keys: i64 = guard
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);
        let tables: i64 = guard
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'journal_events'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
    }

    #[test]
    fn journal_is_append_only() {
        let store = SqliteEdgeStore::open_in_memory().unwrap();
        let guard = store.conn.lock().unwrap();
        guard
            .execute(
                "INSERT INTO journal_events
                     (event_id, event_type, aggregate_id, tenant_id, store_id, occurred_at,
                      payload_json, recorded_at)
                 VALUES ('e', 'X', 'a', 't', 's', 'now', '{}', 'now')",
                [],
            )
            .unwrap();
        assert!(guard
            .execute("DELETE FROM journal_events", [])
            .unwrap_err()
            .to_string()
            .contains("append-only"));
        assert!(guard
            .execute("UPDATE journal_events SET event_type = 'Y'", [])
            .unwrap_err()
            .to_string()
            .contains("append-only"));
    }

    #[test]
    fn pending_outbox_rows_cannot_be_deleted() {
        let store = SqliteEdgeStore::open_in_memory().unwrap();
        let guard = store.conn.lock().unwrap();
        guard
            .execute(
                "INSERT INTO outbox_events
                     (event_id, event_type, aggregate_id, aggregate_version, tenant_id, store_id,
                      occurred_at, payload_json, status, attempt_count, created_at, updated_at)
                 VALUES ('e', 'SALE_FINALIZED', 'a', 1, 't', 's', 'now', '{}', 'PENDING', 0,
                         'now', 'now')",
                [],
            )
            .unwrap();
        assert!(guard
            .execute("DELETE FROM outbox_events", [])
            .unwrap_err()
            .to_string()
            .contains("cannot be deleted"));
    }
}
