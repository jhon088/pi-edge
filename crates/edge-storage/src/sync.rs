//! SQLite adapter for the sync engine's durable state.
//!
//! The outbox rows and the inbound cursor. Three things here are deliberate:
//!
//! * Every state change is guarded by the state it comes *from*
//!   (`AND status = 'SENDING'`), so a stale answer about a fact somebody
//!   already moved updates nothing instead of resurrecting it.
//! * Nothing is ever deleted. Milestone 11 purges neither delivered nor
//!   terminal facts, and migration 0010's trigger makes the second half of
//!   that impossible rather than merely unimplemented (ADR 041).
//! * `seq` comes straight out of SQLite's autoincrement and is read, never
//!   written: it is the installation's durable order, and the sync policy's
//!   determinism rests on it.

use edge_application::ports::{OutboxCounts, SyncUnitOfWork};
use edge_application::AppError;
use edge_domain::prelude::{EventId, OutboxEntry, OutboxStatus, Timestamp};
use rusqlite::{params, params_from_iter, OptionalExtension, Row};

use crate::error::sqlite_err;
use crate::sqlite::{RawOutboxEvent, SqliteUnitOfWork};

const ENTRY_COLUMNS: &str = "seq, event_id, event_type, aggregate_id, aggregate_version, \
     tenant_id, store_id, register_id, business_day_id, occurred_at, payload_json, status, \
     attempt_count, next_attempt_at, last_error_code, last_error_detail";

impl SyncUnitOfWork for SqliteUnitOfWork<'_, '_> {
    fn list_dispatchable_outbox(
        &mut self,
        now: Timestamp,
        limit: i64,
    ) -> Result<Vec<OutboxEntry>, AppError> {
        let sql = format!(
            "SELECT {ENTRY_COLUMNS}
             FROM outbox_events
             WHERE status = 'PENDING'
               AND (next_attempt_at IS NULL OR next_attempt_at <= ?1)
             ORDER BY seq ASC
             LIMIT ?2"
        );
        let mut stmt = self.tx.prepare(&sql).map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![now.to_rfc3339(), limit], raw_entry)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawEntry::into_domain).collect()
    }

    fn list_failed_outbox_aggregates(&mut self) -> Result<Vec<String>, AppError> {
        let mut stmt = self
            .tx
            .prepare("SELECT DISTINCT aggregate_id FROM outbox_events WHERE status = 'FAILED'")
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        Ok(rows)
    }

    fn claim_outbox(
        &mut self,
        event_ids: &[EventId],
        now: Timestamp,
    ) -> Result<Vec<EventId>, AppError> {
        let mut claimed = Vec::with_capacity(event_ids.len());
        for event_id in event_ids {
            let moved = self
                .tx
                .execute(
                    "UPDATE outbox_events
                     SET status = 'SENDING',
                         attempt_count = attempt_count + 1,
                         next_attempt_at = NULL,
                         updated_at = ?2
                     WHERE event_id = ?1 AND status = 'PENDING'",
                    params![event_id.to_string(), now.to_rfc3339()],
                )
                .map_err(sqlite_err)?;
            if moved == 1 {
                claimed.push(*event_id);
            }
        }
        Ok(claimed)
    }

    fn mark_outbox_sent(&mut self, event_id: EventId, now: Timestamp) -> Result<(), AppError> {
        self.tx
            .execute(
                "UPDATE outbox_events
                 SET status = 'SENT',
                     next_attempt_at = NULL,
                     last_error_code = NULL,
                     last_error_detail = NULL,
                     updated_at = ?2
                 WHERE event_id = ?1 AND status = 'SENDING'",
                params![event_id.to_string(), now.to_rfc3339()],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn reschedule_outbox(
        &mut self,
        event_id: EventId,
        next_attempt_at: Timestamp,
        error_code: &str,
        error_detail: &str,
        now: Timestamp,
    ) -> Result<(), AppError> {
        self.tx
            .execute(
                "UPDATE outbox_events
                 SET status = 'PENDING',
                     next_attempt_at = ?2,
                     last_error_code = ?3,
                     last_error_detail = ?4,
                     updated_at = ?5
                 WHERE event_id = ?1 AND status = 'SENDING'",
                params![
                    event_id.to_string(),
                    next_attempt_at.to_rfc3339(),
                    error_code,
                    error_detail,
                    now.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn fail_outbox(
        &mut self,
        event_id: EventId,
        error_code: &str,
        error_detail: &str,
        now: Timestamp,
    ) -> Result<(), AppError> {
        self.tx
            .execute(
                "UPDATE outbox_events
                 SET status = 'FAILED',
                     next_attempt_at = NULL,
                     last_error_code = ?2,
                     last_error_detail = ?3,
                     updated_at = ?4
                 WHERE event_id = ?1 AND status = 'SENDING'",
                params![
                    event_id.to_string(),
                    error_code,
                    error_detail,
                    now.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn recover_sending_outbox(&mut self, now: Timestamp) -> Result<i64, AppError> {
        let recovered = self
            .tx
            .execute(
                "UPDATE outbox_events
                 SET status = 'PENDING',
                     next_attempt_at = NULL,
                     last_error_code = 'SYNC_INTERRUPTED',
                     last_error_detail = 'the dispatcher stopped while this fact was in flight; \
                         it is resent with the same event_id',
                     updated_at = ?1
                 WHERE status = 'SENDING'",
                params![now.to_rfc3339()],
            )
            .map_err(sqlite_err)?;
        Ok(recovered as i64)
    }

    fn outbox_counts(&mut self) -> Result<OutboxCounts, AppError> {
        let mut counts = OutboxCounts::default();
        let mut stmt = self
            .tx
            .prepare("SELECT status, COUNT(*) FROM outbox_events GROUP BY status")
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        for (status, count) in rows {
            match OutboxStatus::parse(&status).map_err(AppError::from)? {
                OutboxStatus::Pending => counts.pending = count,
                OutboxStatus::Sending => counts.sending = count,
                OutboxStatus::Sent => counts.sent = count,
                OutboxStatus::Failed => counts.failed = count,
            }
        }
        Ok(counts)
    }

    fn list_outbox_by_status(
        &mut self,
        status: OutboxStatus,
        limit: i64,
    ) -> Result<Vec<OutboxEntry>, AppError> {
        let sql = format!(
            "SELECT {ENTRY_COLUMNS}
             FROM outbox_events
             WHERE status = ?1
             ORDER BY seq ASC
             LIMIT ?2"
        );
        let mut stmt = self.tx.prepare(&sql).map_err(sqlite_err)?;
        let rows = stmt
            .query_map(
                params_from_iter([status.as_str().to_owned(), limit.to_string()]),
                raw_entry,
            )
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawEntry::into_domain).collect()
    }

    fn inbound_cursor(&mut self) -> Result<Option<String>, AppError> {
        self.tx
            .query_row(
                "SELECT cursor FROM sync_inbound_cursor WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_err)
    }

    fn put_inbound_cursor(&mut self, cursor: &str, now: Timestamp) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO sync_inbound_cursor (id, cursor, updated_at)
                 VALUES (1, ?1, ?2)
                 ON CONFLICT (id) DO UPDATE SET cursor = ?1, updated_at = ?2",
                params![cursor, now.to_rfc3339()],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }
}

/// One outbox row: its durable order, the fact, and why the last attempt
/// failed.
struct RawEntry {
    seq: i64,
    event: RawOutboxEvent,
    last_error_code: Option<String>,
    last_error_detail: Option<String>,
}

/// `seq` first, then the event's own columns, then the failure evidence.
fn raw_entry(row: &Row<'_>) -> rusqlite::Result<RawEntry> {
    Ok(RawEntry {
        seq: row.get(0)?,
        event: RawOutboxEvent::from_offset_row(row, 1)?,
        last_error_code: row.get(14)?,
        last_error_detail: row.get(15)?,
    })
}

impl RawEntry {
    fn into_domain(self) -> Result<OutboxEntry, AppError> {
        Ok(OutboxEntry {
            seq: self.seq,
            event: self.event.into_domain()?,
            last_error_code: self.last_error_code,
            last_error_detail: self.last_error_detail,
        })
    }
}
