-- PI.Edge local schema, version 1.
--
-- Conventions:
--   * public ids are UUID text,
--   * money is INTEGER minor units plus an explicit currency code,
--   * timestamps are RFC-3339 UTC text with microsecond precision
--     (lexicographically sortable),
--   * journal_events and outbox_events are append-only.

-- Device identity, provisioned once. Single row by construction.
CREATE TABLE device_profile (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    tenant_id   TEXT    NOT NULL,
    store_id    TEXT    NOT NULL,
    currency    TEXT    NOT NULL,
    created_at  TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL
);

CREATE TABLE business_days (
    business_day_id TEXT    PRIMARY KEY,
    tenant_id       TEXT    NOT NULL,
    store_id        TEXT    NOT NULL,
    status          TEXT    NOT NULL CHECK (status IN ('OPEN', 'CLOSING', 'CLOSED')),
    opened_at       TEXT    NOT NULL,
    closed_at       TEXT,
    version         INTEGER NOT NULL CHECK (version > 0),
    created_at      TEXT    NOT NULL,
    updated_at      TEXT    NOT NULL,
    CHECK ((status = 'CLOSED') = (closed_at IS NOT NULL))
);

-- Invariant: at most one non-closed business day per store.
CREATE UNIQUE INDEX ux_business_days_active_per_store
    ON business_days (store_id)
    WHERE status <> 'CLOSED';

CREATE INDEX ix_business_days_store_opened
    ON business_days (store_id, opened_at DESC);

CREATE TABLE register_sessions (
    session_id          TEXT    PRIMARY KEY,
    business_day_id     TEXT    NOT NULL REFERENCES business_days (business_day_id),
    register_id         TEXT    NOT NULL,
    operator_id         TEXT    NOT NULL,
    status              TEXT    NOT NULL CHECK (status IN ('OPEN', 'CLOSE_REQUESTED', 'CLOSED')),
    opening_cash_minor  INTEGER NOT NULL CHECK (opening_cash_minor >= 0),
    currency            TEXT    NOT NULL,
    opened_at           TEXT    NOT NULL,
    closed_at           TEXT,
    version             INTEGER NOT NULL CHECK (version > 0),
    created_at          TEXT    NOT NULL,
    updated_at          TEXT    NOT NULL,
    CHECK ((status = 'CLOSED') = (closed_at IS NOT NULL))
);

-- Invariant: a register can only have one live session at a time.
CREATE UNIQUE INDEX ux_register_sessions_active_per_register
    ON register_sessions (register_id)
    WHERE status <> 'CLOSED';

CREATE INDEX ix_register_sessions_business_day
    ON register_sessions (business_day_id, status);

CREATE TABLE retail_transactions (
    transaction_id       TEXT    PRIMARY KEY,
    business_day_id      TEXT    NOT NULL REFERENCES business_days (business_day_id),
    session_id           TEXT    NOT NULL REFERENCES register_sessions (session_id),
    store_id             TEXT    NOT NULL,
    register_id          TEXT    NOT NULL,
    operator_id          TEXT    NOT NULL,
    register_sequence    INTEGER NOT NULL CHECK (register_sequence > 0),
    status               TEXT    NOT NULL CHECK (status IN (
                             'OPEN', 'TOTALIZED', 'PAYMENT_IN_PROGRESS', 'COMMITTED',
                             'RECEIPT_COMPLETED', 'FINALIZED', 'CANCELLED')),
    currency             TEXT    NOT NULL,
    subtotal_minor       INTEGER NOT NULL,
    discount_total_minor INTEGER NOT NULL,
    tax_total_minor      INTEGER NOT NULL,
    grand_total_minor    INTEGER NOT NULL,
    created_at           TEXT    NOT NULL,
    updated_at           TEXT    NOT NULL,
    finalized_at         TEXT,
    version              INTEGER NOT NULL CHECK (version > 0),
    CHECK ((status = 'FINALIZED') = (finalized_at IS NOT NULL)),
    UNIQUE (register_id, register_sequence)
);

CREATE INDEX ix_retail_transactions_session_status
    ON retail_transactions (session_id, status);

CREATE INDEX ix_retail_transactions_business_day
    ON retail_transactions (business_day_id, created_at);

CREATE TABLE transaction_lines (
    line_id               TEXT    PRIMARY KEY,
    transaction_id        TEXT    NOT NULL REFERENCES retail_transactions (transaction_id)
                                  ON DELETE CASCADE,
    line_number           INTEGER NOT NULL CHECK (line_number > 0),
    product_ref           TEXT    NOT NULL,
    sku                   TEXT,
    barcode               TEXT,
    description           TEXT    NOT NULL,
    quantity_thousandths  INTEGER NOT NULL CHECK (quantity_thousandths > 0),
    unit_price_minor      INTEGER NOT NULL CHECK (unit_price_minor >= 0),
    tax_rate_bps          INTEGER NOT NULL CHECK (tax_rate_bps >= 0),
    gross_amount_minor    INTEGER NOT NULL,
    discount_amount_minor INTEGER NOT NULL CHECK (discount_amount_minor >= 0),
    taxable_amount_minor  INTEGER NOT NULL CHECK (taxable_amount_minor >= 0),
    tax_amount_minor      INTEGER NOT NULL,
    line_total_minor      INTEGER NOT NULL,
    currency              TEXT    NOT NULL,
    created_at            TEXT    NOT NULL,
    UNIQUE (transaction_id, line_number)
);

CREATE INDEX ix_transaction_lines_transaction
    ON transaction_lines (transaction_id, line_number);

CREATE TABLE payments (
    payment_id     TEXT    PRIMARY KEY,
    transaction_id TEXT    NOT NULL REFERENCES retail_transactions (transaction_id)
                           ON DELETE CASCADE,
    tender_type    TEXT    NOT NULL CHECK (tender_type IN ('CASH')),
    amount_minor   INTEGER NOT NULL CHECK (amount_minor > 0),
    currency       TEXT    NOT NULL,
    status         TEXT    NOT NULL CHECK (status IN ('CREATED', 'COMMITTED')),
    created_at     TEXT    NOT NULL,
    committed_at   TEXT,
    CHECK ((status = 'COMMITTED') = (committed_at IS NOT NULL))
);

CREATE INDEX ix_payments_transaction ON payments (transaction_id);

-- Append-only audit trail.
CREATE TABLE journal_events (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id        TEXT    NOT NULL UNIQUE,
    event_type      TEXT    NOT NULL,
    aggregate_id    TEXT    NOT NULL,
    tenant_id       TEXT    NOT NULL,
    store_id        TEXT    NOT NULL,
    register_id     TEXT,
    business_day_id TEXT,
    session_id      TEXT,
    transaction_id  TEXT,
    operator_id     TEXT,
    occurred_at     TEXT    NOT NULL,
    payload_json    TEXT    NOT NULL,
    recorded_at     TEXT    NOT NULL
);

CREATE INDEX ix_journal_events_business_day ON journal_events (business_day_id, seq);
CREATE INDEX ix_journal_events_transaction ON journal_events (transaction_id, seq);
CREATE INDEX ix_journal_events_type ON journal_events (event_type, seq);

CREATE TRIGGER trg_journal_events_no_update
BEFORE UPDATE ON journal_events
BEGIN
    SELECT RAISE(ABORT, 'journal_events is append-only');
END;

CREATE TRIGGER trg_journal_events_no_delete
BEFORE DELETE ON journal_events
BEGIN
    SELECT RAISE(ABORT, 'journal_events is append-only');
END;

-- At-least-once hand-off to PI.COMMERCE Server. event_id is the server-side
-- idempotency key.
CREATE TABLE outbox_events (
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
    status            TEXT    NOT NULL CHECK (status IN ('PENDING', 'SENDING', 'SENT')),
    attempt_count     INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    next_attempt_at   TEXT,
    created_at        TEXT    NOT NULL,
    updated_at        TEXT    NOT NULL
);

CREATE INDEX ix_outbox_events_status ON outbox_events (status, seq);
CREATE INDEX ix_outbox_events_next_attempt ON outbox_events (status, next_attempt_at);

-- Undelivered work must never be dropped silently.
CREATE TRIGGER trg_outbox_events_keep_pending
BEFORE DELETE ON outbox_events
WHEN OLD.status <> 'SENT'
BEGIN
    SELECT RAISE(ABORT, 'outbox_events rows that are not SENT cannot be deleted');
END;

-- Explicit command idempotency.
CREATE TABLE processed_commands (
    request_id   TEXT PRIMARY KEY,
    command_type TEXT NOT NULL,
    result_json  TEXT NOT NULL,
    processed_at TEXT NOT NULL
);
