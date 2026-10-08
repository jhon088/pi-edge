//! The SQLite adapter: `EdgeStore` + `UnitOfWork` implementations.

use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Duration;

use edge_application::ports::{DeviceProfile, EdgeStore, UnitOfWork};
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
    #[cfg(feature = "fault-injection")]
    fail_before_commit: std::sync::atomic::AtomicBool,
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
            fail_before_commit: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Test hook: abort the next unit of work after the writes but **before**
    /// `COMMIT`, simulating a power loss at the worst possible moment.
    #[cfg(feature = "fault-injection")]
    pub fn arm_commit_failure(&self) {
        self.fail_before_commit
            .store(true, std::sync::atomic::Ordering::SeqCst);
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
            .swap(false, std::sync::atomic::Ordering::SeqCst)
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
                "SELECT tenant_id, store_id, currency FROM device_profile WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(sqlite_err)?;
        match row {
            None => Ok(None),
            Some((tenant_id, store_id, currency)) => Ok(Some(DeviceProfile {
                tenant_id: parse_id(&tenant_id)?,
                store_id: parse_id(&store_id)?,
                currency: parse_currency(&currency)?,
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
                "INSERT INTO device_profile (id, tenant_id, store_id, currency, created_at, updated_at)
                 VALUES (1, ?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT(id) DO UPDATE SET
                     tenant_id = excluded.tenant_id,
                     store_id = excluded.store_id,
                     currency = excluded.currency,
                     updated_at = excluded.updated_at",
                params![
                    profile.tenant_id.as_str(),
                    profile.store_id.as_str(),
                    profile.currency.code(),
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
                "SELECT business_day_id, tenant_id, store_id, status, opened_at, closed_at, version
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
                "SELECT business_day_id, tenant_id, store_id, status, opened_at, closed_at, version
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
                 SET status = ?2, closed_at = ?3, version = ?4, updated_at = ?5
                 WHERE business_day_id = ?1 AND version < ?4",
                params![
                    state.business_day_id.to_string(),
                    state.status.as_str(),
                    state.closed_at.map(|ts| ts.to_rfc3339()),
                    state.version,
                    now.to_rfc3339(),
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
                        opening_cash_minor, currency, opened_at, closed_at, version
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
                        opening_cash_minor, currency, opened_at, closed_at, version
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
                      created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
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
                 SET status = ?2, closed_at = ?3, version = ?4, updated_at = ?5
                 WHERE session_id = ?1 AND version < ?4",
                params![
                    state.session_id.to_string(),
                    state.status.as_str(),
                    state.closed_at.map(|ts| ts.to_rfc3339()),
                    state.version,
                    now.to_rfc3339(),
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
                        catalog_snapshot_id, catalog_version
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
                        catalog_snapshot_id, catalog_version
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
                      catalog_snapshot_id, catalog_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?20)",
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
                    state.catalog.map(|catalog| catalog.snapshot_id.to_string()),
                    state.catalog.map(|catalog| catalog.version),
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
                     updated_at = ?8, finalized_at = ?9, version = ?10
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
    /// Lines are immutable once priced; payments can only gain their commit
    /// timestamp, so both are written with upserts.
    fn write_children(
        &self,
        transaction: &RetailTransaction,
        now: Timestamp,
    ) -> Result<(), AppError> {
        let state = transaction.state();
        for line in &state.lines {
            self.tx
                .execute(
                    "INSERT INTO transaction_lines
                         (line_id, transaction_id, line_number, product_ref, sku, barcode,
                          description, quantity_thousandths, unit_price_minor, tax_rate_bps,
                          price_mode, extended_amount_minor, discount_amount_minor,
                          taxable_amount_minor, tax_amount_minor, line_total_minor, currency,
                          created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                             ?16, ?17, ?18)
                     ON CONFLICT(line_id) DO NOTHING",
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
        for payment in &state.payments {
            self.tx
                .execute(
                    "INSERT INTO payments
                         (payment_id, transaction_id, tender_type, amount_minor, currency,
                          status, created_at, committed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(payment_id) DO UPDATE SET
                         status = excluded.status,
                         committed_at = excluded.committed_at",
                    params![
                        payment.payment_id.to_string(),
                        state.transaction_id.to_string(),
                        payment.tender_type.as_str(),
                        payment.amount.minor(),
                        payment.amount.currency().code(),
                        payment.status.as_str(),
                        payment.created_at.to_rfc3339(),
                        payment.committed_at.map(|ts| ts.to_rfc3339()),
                    ],
                )
                .map_err(sqlite_err)?;
        }
        Ok(())
    }

    fn load_transaction(&self, raw: RawTransaction) -> Result<RetailTransaction, AppError> {
        let transaction_id: TransactionId = parse_id(&raw.transaction_id)?;
        let lines = self.load_lines(&raw.transaction_id)?;
        let payments = self.load_payments(&raw.transaction_id, transaction_id)?;
        raw.into_domain(lines, payments)
    }

    fn load_lines(&self, transaction_id: &str) -> Result<Vec<TransactionLine>, AppError> {
        let mut stmt = self
            .tx
            .prepare(
                "SELECT line_id, line_number, product_ref, sku, barcode, description,
                        quantity_thousandths, unit_price_minor, tax_rate_bps, price_mode,
                        extended_amount_minor, discount_amount_minor, taxable_amount_minor,
                        tax_amount_minor, line_total_minor, currency
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

    fn load_payments(
        &self,
        transaction_id: &str,
        parsed_transaction_id: TransactionId,
    ) -> Result<Vec<Payment>, AppError> {
        let mut stmt = self
            .tx
            .prepare(
                "SELECT payment_id, tender_type, amount_minor, currency, status,
                        created_at, committed_at
                 FROM payments
                 WHERE transaction_id = ?1
                 ORDER BY created_at ASC, payment_id ASC",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![transaction_id], RawPayment::from_row)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter()
            .map(|raw| raw.into_domain(parsed_transaction_id))
            .collect()
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
            version: self.version,
        }))
    }
}

struct RawSession {
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
}

impl RawSession {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
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
        })
    }

    fn into_domain(self) -> Result<RegisterSession, AppError> {
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
            version: self.version,
        }))
    }
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
        })
    }

    fn into_domain(
        self,
        lines: Vec<TransactionLine>,
        payments: Vec<Payment>,
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
            catalog: match (self.catalog_snapshot_id, self.catalog_version) {
                (Some(snapshot_id), Some(version)) => Some(CatalogRef {
                    snapshot_id: parse_id(&snapshot_id)?,
                    version,
                }),
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
            payments,
            created_at: parse_ts(&self.created_at)?,
            updated_at: parse_ts(&self.updated_at)?,
            finalized_at: parse_opt_ts(self.finalized_at)?,
            version: self.version,
        }))
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
            extended_amount_minor: row.get(10)?,
            discount_amount_minor: row.get(11)?,
            taxable_amount_minor: row.get(12)?,
            tax_amount_minor: row.get(13)?,
            line_total_minor: row.get(14)?,
            currency: row.get(15)?,
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
            extended_amount: money(self.extended_amount_minor, currency),
            discount_amount: money(self.discount_amount_minor, currency),
            taxable_amount: money(self.taxable_amount_minor, currency),
            tax_amount: money(self.tax_amount_minor, currency),
            line_total: money(self.line_total_minor, currency),
        })
    }
}

struct RawPayment {
    payment_id: String,
    tender_type: String,
    amount_minor: i64,
    currency: String,
    status: String,
    created_at: String,
    committed_at: Option<String>,
}

impl RawPayment {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            payment_id: row.get(0)?,
            tender_type: row.get(1)?,
            amount_minor: row.get(2)?,
            currency: row.get(3)?,
            status: row.get(4)?,
            created_at: row.get(5)?,
            committed_at: row.get(6)?,
        })
    }

    fn into_domain(self, transaction_id: TransactionId) -> Result<Payment, AppError> {
        let currency = parse_currency(&self.currency)?;
        Ok(Payment {
            payment_id: parse_id(&self.payment_id)?,
            transaction_id,
            tender_type: TenderType::parse(&self.tender_type).map_err(AppError::from)?,
            amount: money(self.amount_minor, currency),
            status: PaymentStatus::parse(&self.status).map_err(AppError::from)?,
            created_at: parse_ts(&self.created_at)?,
            committed_at: parse_opt_ts(self.committed_at)?,
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

struct RawOutboxEvent {
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
        Ok(Self {
            event_id: row.get(0)?,
            event_type: row.get(1)?,
            aggregate_id: row.get(2)?,
            aggregate_version: row.get(3)?,
            tenant_id: row.get(4)?,
            store_id: row.get(5)?,
            register_id: row.get(6)?,
            business_day_id: row.get(7)?,
            occurred_at: row.get(8)?,
            payload_json: row.get(9)?,
            status: row.get(10)?,
            attempt_count: row.get(11)?,
            next_attempt_at: row.get(12)?,
        })
    }

    fn into_domain(self) -> Result<OutboxEvent, AppError> {
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
