-- Milestone 12 — the receipt is an obligation, not a transaction status.
--
-- Two changes, and the first one is a removal:
--
--   1. `RECEIPT_COMPLETED` leaves `retail_transactions.status`. It was written
--      into migration 0001 assuming the order *pay -> print -> close*, and
--      milestone 10 froze the opposite: a physical side effect never
--      participates in a commercial transaction (ADR 037). Nothing ever
--      entered the state, and entering it would have put the printer inside
--      the sale's close. `FINALIZED` now follows `COMMITTED` and nothing else
--      (ADR 043).
--   2. `transaction_receipts` arrives: one row per finalized sale saying that
--      the sale *owes* a receipt and what became of it.
--
-- The rebuilt table carries every column the sale has today, including the
-- five that later migrations added: a rebuild that copies only migration
-- 0001's shape silently drops the catalog binding, the tax base and the line
-- counter.
--
-- Narrowing a CHECK means rebuilding the table, and seven tables carry a
-- foreign key into `retail_transactions`. The migration runner therefore turns
-- foreign-key enforcement off for the duration — SQLite's own prescribed
-- procedure — restores it afterwards, and refuses to finish if any child row
-- was left orphaned.

-- A legacy row in the removed state would have no honest home: it is not
-- `FINALIZED` (no `finalized_at`), and inventing a conversion would be writing
-- history. So the migration refuses to run, loudly, rather than guessing.
CREATE TABLE _m0011_legacy_guard (rows_in_removed_state INTEGER NOT NULL);

CREATE TRIGGER _m0011_refuse_legacy_receipt_completed
BEFORE INSERT ON _m0011_legacy_guard
WHEN NEW.rows_in_removed_state > 0
BEGIN
    SELECT RAISE(ABORT, 'migration 0011 refuses to run: retail_transactions still holds rows in the removed RECEIPT_COMPLETED state. There is no conversion to FINALIZED and the state is not preserved; resolve those sales before upgrading');
END;

INSERT INTO _m0011_legacy_guard (rows_in_removed_state)
SELECT COUNT(*) FROM retail_transactions WHERE status = 'RECEIPT_COMPLETED';

DROP TRIGGER _m0011_refuse_legacy_receipt_completed;
DROP TABLE _m0011_legacy_guard;

-- The rebuild. Same columns, same checks, same unique key, one fewer status.
--
-- The order matters and is SQLite's prescribed one: build the replacement
-- under a temporary name, copy, drop the original, then rename into place.
-- Renaming the *original* out of the way first would rewrite the `REFERENCES`
-- clause of all seven child tables to point at the table about to be dropped,
-- which is how a rebuild quietly orphans its children. Dropping the original
-- is safe only because enforcement is off for the migration run: five of those
-- children cascade on delete, and `transaction_lines` would have taken every
-- line of every sale with it.
CREATE TABLE retail_transactions_v11 (
    transaction_id       TEXT    PRIMARY KEY,
    business_day_id      TEXT    NOT NULL REFERENCES business_days (business_day_id),
    session_id           TEXT    NOT NULL REFERENCES register_sessions (session_id),
    store_id             TEXT    NOT NULL,
    register_id          TEXT    NOT NULL,
    operator_id          TEXT    NOT NULL,
    register_sequence    INTEGER NOT NULL CHECK (register_sequence > 0),
    status               TEXT    NOT NULL CHECK (status IN (
                             'OPEN', 'TOTALIZED', 'PAYMENT_IN_PROGRESS', 'COMMITTED',
                             'FINALIZED', 'CANCELLED')),
    currency             TEXT    NOT NULL,
    subtotal_minor       INTEGER NOT NULL,
    discount_total_minor INTEGER NOT NULL,
    tax_total_minor      INTEGER NOT NULL,
    grand_total_minor    INTEGER NOT NULL,
    created_at           TEXT    NOT NULL,
    updated_at           TEXT    NOT NULL,
    finalized_at         TEXT,
    version              INTEGER NOT NULL CHECK (version > 0),
    -- Added by 0002: the snapshot that priced this sale, which it keeps
    -- resolving against for its whole life (ADR 013).
    catalog_snapshot_id  TEXT    REFERENCES catalog_snapshots (snapshot_id),
    catalog_version      INTEGER,
    -- Added by 0002: the tax base, so a receipt can print
    -- subtotal + tax = total in both price modes.
    net_total_minor      INTEGER NOT NULL DEFAULT 0,
    -- Added by 0003: which instant the sale resolved its prices at.
    pricing_effective_at TEXT,
    -- Added by 0004: line numbers are never reused, so this survives a reload.
    next_line_number     INTEGER NOT NULL DEFAULT 1,
    CHECK ((status = 'FINALIZED') = (finalized_at IS NOT NULL)),
    UNIQUE (register_id, register_sequence)
);

INSERT INTO retail_transactions_v11
    (transaction_id, business_day_id, session_id, store_id, register_id, operator_id,
     register_sequence, status, currency, subtotal_minor, discount_total_minor,
     tax_total_minor, grand_total_minor, created_at, updated_at, finalized_at, version,
     catalog_snapshot_id, catalog_version, net_total_minor, pricing_effective_at,
     next_line_number)
SELECT
     transaction_id, business_day_id, session_id, store_id, register_id, operator_id,
     register_sequence, status, currency, subtotal_minor, discount_total_minor,
     tax_total_minor, grand_total_minor, created_at, updated_at, finalized_at, version,
     catalog_snapshot_id, catalog_version, net_total_minor, pricing_effective_at,
     next_line_number
FROM retail_transactions;

DROP TABLE retail_transactions;

ALTER TABLE retail_transactions_v11 RENAME TO retail_transactions;

CREATE INDEX ix_retail_transactions_session_status
    ON retail_transactions (session_id, status);

CREATE INDEX ix_retail_transactions_business_day
    ON retail_transactions (business_day_id, created_at);

-- Recent sales, newest first: what an operator needs to find the sale they
-- just made in order to reprint it.
CREATE INDEX ix_retail_transactions_finalized
    ON retail_transactions (finalized_at DESC)
    WHERE finalized_at IS NOT NULL;

-- What this sale owes the customer in paper, and what became of it.
--
-- Deliberately **not** `hardware_actions`. That table records what we
-- attempted on a device; this one records a commercial obligation. One sale
-- can produce several device actions — a failed first attempt, a retry, any
-- number of reprints — and still have exactly one receipt obligation
-- (ADR 043).
CREATE TABLE transaction_receipts (
    transaction_id    TEXT    PRIMARY KEY
                              REFERENCES retail_transactions (transaction_id),
    -- PENDING  the sale owes a receipt and no attempt has resolved
    -- PRINTED  the device confirmed it
    -- FAILED   the device certainly did not print: retry is safe
    -- UNKNOWN  paper may have come out. No automatic retry, ever (ADR 043)
    status            TEXT    NOT NULL CHECK (status IN
                                  ('PENDING', 'PRINTED', 'FAILED', 'UNKNOWN')),
    attempt_count     INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    -- Set once, and only on a confirmed success.
    first_printed_at  TEXT,
    last_attempt_at   TEXT,
    last_error_code   TEXT,
    last_error_detail TEXT,
    created_at        TEXT    NOT NULL,
    updated_at        TEXT    NOT NULL,
    -- Only a printed receipt has a printing time, and only a resolved one
    -- has been attempted.
    CHECK ((status = 'PRINTED') = (first_printed_at IS NOT NULL)),
    CHECK (status = 'PENDING' OR attempt_count > 0)
);

-- The attention list: what an operator still has to deal with. `PRINTED` is
-- not in it, which is the point.
CREATE INDEX ix_transaction_receipts_attention
    ON transaction_receipts (status)
    WHERE status <> 'PRINTED';
