-- Milestone 11 — sync engine.
--
-- Two changes, both to the outbound contract that migration 0001 fixed before
-- there was anything to dispatch:
--
--   1. `FAILED` joins the status set. A fact the consumer rejects for good must
--      not be retried forever and must not be quietly dropped, so it becomes
--      terminal and visible, with the reason attached (ADR 041).
--   2. The inbound side gets a durable cursor, written in the same transaction
--      as the effect it accounts for (ADR 041).
--
-- `outbox_events` is rebuilt because SQLite cannot widen a CHECK constraint in
-- place. Every row, its `seq`, both indexes and the delete guard are carried
-- over; `seq` values are copied explicitly, so the local durable order that
-- governs batch formation survives the rebuild unchanged. The table has no
-- foreign keys in either direction, which is what makes a plain rebuild safe
-- inside the migration transaction.

DROP TRIGGER trg_outbox_events_keep_pending;
DROP INDEX ix_outbox_events_status;
DROP INDEX ix_outbox_events_next_attempt;

CREATE TABLE outbox_events_rebuilt (
    seq               INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id          TEXT    NOT NULL UNIQUE,
    event_type        TEXT    NOT NULL,
    aggregate_id      TEXT    NOT NULL,
    aggregate_version INTEGER NOT NULL CHECK (aggregate_version > 0),
    tenant_id         TEXT    NOT NULL,
    store_id          TEXT    NOT NULL,
    register_id       TEXT,
    business_day_id   TEXT,
    occurred_at       TEXT    NOT NULL,
    payload_json      TEXT    NOT NULL,
    status            TEXT    NOT NULL CHECK (status IN ('PENDING', 'SENDING', 'SENT', 'FAILED')),
    attempt_count     INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    next_attempt_at   TEXT,
    -- Why the last attempt did not land. Kept for a transient failure too:
    -- an operator looking at a fact stuck pending needs the reason, not just
    -- the attempt count.
    last_error_code   TEXT,
    last_error_detail TEXT,
    created_at        TEXT    NOT NULL,
    updated_at        TEXT    NOT NULL,
    -- A terminal fact always carries its reason, or it is not actionable.
    CHECK (status <> 'FAILED' OR last_error_code IS NOT NULL)
);

INSERT INTO outbox_events_rebuilt
    (seq, event_id, event_type, aggregate_id, aggregate_version, tenant_id, store_id,
     register_id, business_day_id, occurred_at, payload_json, status, attempt_count,
     next_attempt_at, last_error_code, last_error_detail, created_at, updated_at)
SELECT
    seq, event_id, event_type, aggregate_id, aggregate_version, tenant_id, store_id,
    register_id, business_day_id, occurred_at, payload_json, status, attempt_count,
    next_attempt_at, NULL, NULL, created_at, updated_at
FROM outbox_events;

DROP TABLE outbox_events;
ALTER TABLE outbox_events_rebuilt RENAME TO outbox_events;

CREATE INDEX ix_outbox_events_status ON outbox_events (status, seq);
CREATE INDEX ix_outbox_events_next_attempt ON outbox_events (status, next_attempt_at);

-- Unchanged in substance: anything not confirmed delivered cannot be deleted.
-- `FAILED` now falls under the same guard, which is deliberate — milestone 11
-- purges nothing at all, and a rejected fact is exactly the one somebody will
-- need to look at later.
CREATE TRIGGER trg_outbox_events_keep_pending
BEFORE DELETE ON outbox_events
WHEN OLD.status <> 'SENT'
BEGIN
    SELECT RAISE(ABORT, 'outbox_events rows that are not SENT cannot be deleted');
END;

-- How far this installation has consumed the inbound stream.
--
-- One row. It advances only in the transaction that made the corresponding
-- effect durable, so the cursor can never be ahead of the local state: a crash
-- re-delivers a message instead of skipping it.
CREATE TABLE sync_inbound_cursor (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    cursor     TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
