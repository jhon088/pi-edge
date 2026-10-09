-- PI.Edge local schema, version 8: commercial refunds.
--
-- A refund is its own document, not a correction of the sale (ADR 034). What
-- it materialises is the economics it gave back, so an audit never depends on
-- re-pricing the original sale against a catalog that may be gone (ADR 031).

-- ------------------------------------------------------------ fiscal metadata

-- Whether a sale was invoiced, kept apart from the sale itself.
--
-- A sale can be finalized today and invoiced tomorrow, so this has a lifecycle
-- of its own and does not belong on an otherwise immutable finalized sale
-- (ADR 032). One row per finalized sale, written when the sale finalizes, so a
-- missing row for a finalized sale is a bug rather than a third state.
CREATE TABLE transaction_fiscal_metadata (
    transaction_id TEXT    PRIMARY KEY REFERENCES retail_transactions (transaction_id)
                           ON DELETE CASCADE,
    fiscal_state   TEXT    NOT NULL CHECK (fiscal_state IN ('NOT_INVOICED', 'INVOICED')),
    -- Canonical, opaque reference to the invoice, assigned upstream. Never a
    -- PAC payload, never an ERP document id, never a CFDI field.
    invoice_ref    TEXT,
    updated_at     TEXT    NOT NULL,
    -- An invoiced sale names its invoice; a sale that was never invoiced has
    -- nothing to name.
    CHECK ((fiscal_state = 'INVOICED') = (invoice_ref IS NOT NULL))
);

-- ------------------------------------------------------------------- refunds

CREATE TABLE refunds (
    refund_id               TEXT    PRIMARY KEY,
    original_transaction_id TEXT    NOT NULL REFERENCES retail_transactions (transaction_id),
    store_id                TEXT    NOT NULL,
    -- The day and session the refund happened in, never the sale's: a refund
    -- is money and goods moving today.
    business_day_id         TEXT    NOT NULL REFERENCES business_days (business_day_id),
    session_id              TEXT    NOT NULL REFERENCES register_sessions (session_id),
    register_id             TEXT    NOT NULL,
    operator_id             TEXT    NOT NULL,
    status                  TEXT    NOT NULL CHECK (status IN
                                        ('CREATED', 'ACCEPTED', 'FINANCIAL_PENDING',
                                         'COMPLETED', 'FAILED')),
    -- Evidence that the original sale may be refunded. PI.Edge never assumes
    -- it from a locally finalized sale (ADR 030).
    authority               TEXT    NOT NULL CHECK (authority IN ('CENTRAL_ACCEPTED')),
    authorization_ref       TEXT,
    currency                TEXT    NOT NULL,
    -- The economics given back, materialised. `gross = net + tax` always.
    extended_minor          INTEGER NOT NULL CHECK (extended_minor >= 0),
    discount_minor          INTEGER NOT NULL CHECK (discount_minor >= 0),
    net_minor               INTEGER NOT NULL CHECK (net_minor >= 0),
    tax_minor               INTEGER NOT NULL CHECK (tax_minor >= 0),
    gross_minor             INTEGER NOT NULL CHECK (gross_minor >= 0),
    fiscal_status           TEXT    NOT NULL CHECK (fiscal_status IN
                                        ('NONE', 'CREDIT_NOTE_PENDING',
                                         'CREDIT_NOTE_REQUESTED')),
    -- The original's fiscal state at the time of the refund, so a later
    -- invoice cannot rewrite the history of this decision.
    original_fiscal_state   TEXT    NOT NULL CHECK (original_fiscal_state IN
                                        ('NOT_INVOICED', 'INVOICED')),
    reason_code             TEXT    NOT NULL CHECK (TRIM(reason_code) <> ''),
    -- V1 returns everything to sellable stock; the column exists so a later
    -- disposition is a value rather than a schema change (ADR 033).
    restock                 INTEGER NOT NULL DEFAULT 1 CHECK (restock IN (0, 1)),
    created_at              TEXT    NOT NULL,
    updated_at              TEXT    NOT NULL,
    accepted_at             TEXT,
    completed_at            TEXT,
    version                 INTEGER NOT NULL CHECK (version > 0),
    CHECK (gross_minor = net_minor + tax_minor),
    -- A refund that was accepted says when, and one that was not cannot.
    CHECK ((status IN ('ACCEPTED', 'FINANCIAL_PENDING', 'COMPLETED')) = (accepted_at IS NOT NULL)),
    CHECK ((status IN ('COMPLETED', 'FAILED')) = (completed_at IS NOT NULL)),
    -- A sale that was never invoiced has no credit note to speak of, and one
    -- that was has to have chosen (ADR 032).
    CHECK (
        CASE original_fiscal_state
            WHEN 'NOT_INVOICED' THEN fiscal_status = 'NONE'
            WHEN 'INVOICED'     THEN fiscal_status IN ('CREDIT_NOTE_PENDING',
                                                       'CREDIT_NOTE_REQUESTED')
        END
    )
);

-- One refund in progress per original sale (ADR 030). A completed refund does
-- not block a later one while quantity remains; a live one does, and that is
-- what stops two tills from giving the same goods back twice.
CREATE UNIQUE INDEX ux_refunds_in_progress
    ON refunds (original_transaction_id)
    WHERE status NOT IN ('COMPLETED', 'FAILED');

CREATE INDEX ix_refunds_original ON refunds (original_transaction_id, created_at);
CREATE INDEX ix_refunds_session ON refunds (session_id, created_at);
CREATE INDEX ix_refunds_day ON refunds (business_day_id, created_at);

CREATE TABLE refund_lines (
    refund_line_id       TEXT    PRIMARY KEY,
    refund_id            TEXT    NOT NULL REFERENCES refunds (refund_id) ON DELETE CASCADE,
    -- The original line this gives back. Line numbers are never reused, so it
    -- always points at the same goods.
    original_line_number INTEGER NOT NULL CHECK (original_line_number > 0),
    product_ref          TEXT    NOT NULL,
    description          TEXT    NOT NULL,
    quantity_thousandths INTEGER NOT NULL CHECK (quantity_thousandths > 0),
    -- Carried for audit only. The refund is computed from the materialised
    -- amounts below, never from a unit price (ADR 031).
    unit_price_minor     INTEGER NOT NULL CHECK (unit_price_minor >= 0),
    extended_minor       INTEGER NOT NULL CHECK (extended_minor >= 0),
    discount_minor       INTEGER NOT NULL CHECK (discount_minor >= 0),
    net_minor            INTEGER NOT NULL CHECK (net_minor >= 0),
    tax_minor            INTEGER NOT NULL CHECK (tax_minor >= 0),
    gross_minor          INTEGER NOT NULL CHECK (gross_minor >= 0),
    CHECK (gross_minor = net_minor + tax_minor),
    -- One refund names one original line once; several refunds may name it.
    UNIQUE (refund_id, original_line_number)
);

CREATE INDEX ix_refund_lines_refund ON refund_lines (refund_id, original_line_number);

-- ----------------------------------------------------------- financial refund

CREATE TABLE refund_payments (
    refund_payment_id   TEXT    PRIMARY KEY,
    refund_id           TEXT    NOT NULL REFERENCES refunds (refund_id) ON DELETE CASCADE,
    -- The tender this gives back through, so a tender never returns more than
    -- it took (ADR 031).
    original_payment_id TEXT    NOT NULL REFERENCES payments (payment_id),
    tender_type         TEXT    NOT NULL CHECK (tender_type IN ('CASH', 'CARD')),
    requested_minor     INTEGER NOT NULL CHECK (requested_minor > 0),
    refunded_minor      INTEGER NOT NULL DEFAULT 0 CHECK (refunded_minor >= 0),
    status              TEXT    NOT NULL CHECK (status IN
                                    ('CREATED', 'PENDING', 'REFUNDED', 'FAILED')),
    currency            TEXT    NOT NULL,
    created_at          TEXT    NOT NULL,
    updated_at          TEXT    NOT NULL,
    -- Nothing goes back until it settles, and never more than was asked.
    CHECK (refunded_minor <= requested_minor),
    CHECK ((status = 'REFUNDED') = (refunded_minor > 0)),
    -- One refund gives back through one original tender once.
    UNIQUE (refund_id, original_payment_id)
);

CREATE INDEX ix_refund_payments_refund ON refund_payments (refund_id, created_at);

-- Append-only, like a payment attempt: a reconciliation adds knowledge, it
-- never rewrites what was sent.
CREATE TABLE refund_attempts (
    attempt_id         TEXT    PRIMARY KEY,
    refund_payment_id  TEXT    NOT NULL REFERENCES refund_payments (refund_payment_id)
                               ON DELETE CASCADE,
    attempt_number     INTEGER NOT NULL CHECK (attempt_number > 0),
    status             TEXT    NOT NULL CHECK (status IN
                                   ('CREATED', 'PROCESSING', 'UNKNOWN', 'REFUNDED', 'FAILED')),
    requested_minor    INTEGER NOT NULL CHECK (requested_minor > 0),
    refunded_minor     INTEGER CHECK (refunded_minor IS NULL OR refunded_minor >= 0),
    currency           TEXT    NOT NULL,
    -- Minted and committed before the provider is called, and unique across
    -- the installation: it is what the provider is asked about, and what makes
    -- a provider-side retry idempotent (ADR 020).
    correlation_id     TEXT    NOT NULL UNIQUE,
    provider_reference TEXT,
    failure_code       TEXT,
    created_at         TEXT    NOT NULL,
    updated_at         TEXT    NOT NULL,
    UNIQUE (refund_payment_id, attempt_number)
);

CREATE INDEX ix_refund_attempts_payment
    ON refund_attempts (refund_payment_id, attempt_number);

CREATE TRIGGER trg_refund_attempts_no_delete
BEFORE DELETE ON refund_attempts
BEGIN
    SELECT RAISE(ABORT, 'refund attempts are append-only');
END;

-- ------------------------------------------- the two ledgers gain one type each
--
-- SQLite cannot alter a CHECK constraint, so a table that gains a movement
-- type is rebuilt. Both ledgers stay append-only: the triggers are recreated
-- with the table, and the copy preserves every existing row.

-- Cash: CASH_REFUND, direction OUT. Cash handed back for a refund, exactly
-- once per financial refund, keyed by the refund payment that caused it.
ALTER TABLE cash_movements RENAME TO cash_movements_v6;

CREATE TABLE cash_movements (
    movement_id     TEXT    PRIMARY KEY,
    business_day_id TEXT    NOT NULL REFERENCES business_days (business_day_id),
    session_id      TEXT    NOT NULL REFERENCES register_sessions (session_id),
    register_id     TEXT    NOT NULL,
    operator_id     TEXT    NOT NULL,
    movement_type   TEXT    NOT NULL CHECK (movement_type IN
                                ('OPENING_FLOAT', 'CASH_SALE', 'CASH_IN', 'CASH_OUT',
                                 'SAFE_DROP', 'CASH_REFUND')),
    direction       TEXT    NOT NULL CHECK (direction IN ('IN', 'OUT')),
    amount_minor    INTEGER NOT NULL CHECK (amount_minor > 0),
    currency        TEXT    NOT NULL,
    tendered_minor  INTEGER CHECK (tendered_minor IS NULL OR tendered_minor > 0),
    change_minor    INTEGER CHECK (change_minor IS NULL OR change_minor >= 0),
    source_ref      TEXT,
    reason_code     TEXT,
    approval_id     TEXT    REFERENCES approvals (approval_id),
    created_at      TEXT    NOT NULL,
    CHECK (
        CASE movement_type
            WHEN 'OPENING_FLOAT' THEN direction = 'IN'
            WHEN 'CASH_SALE'     THEN direction = 'IN'
            WHEN 'CASH_IN'       THEN direction = 'IN'
            WHEN 'CASH_OUT'      THEN direction = 'OUT'
            WHEN 'SAFE_DROP'     THEN direction = 'OUT'
            WHEN 'CASH_REFUND'   THEN direction = 'OUT'
        END
    ),
    CHECK ((movement_type = 'CASH_SALE') = (tendered_minor IS NOT NULL)),
    CHECK ((tendered_minor IS NULL) = (change_minor IS NULL)),
    CHECK (tendered_minor IS NULL OR amount_minor = tendered_minor - change_minor),
    CHECK (movement_type NOT IN ('CASH_IN', 'CASH_OUT')
           OR (reason_code IS NOT NULL AND TRIM(reason_code) <> '')),
    -- A refund movement names the refund payment it settles, which is what
    -- keeps it to exactly one movement per financial refund.
    CHECK (movement_type <> 'CASH_REFUND' OR source_ref IS NOT NULL)
);

INSERT INTO cash_movements
    (movement_id, business_day_id, session_id, register_id, operator_id, movement_type,
     direction, amount_minor, currency, tendered_minor, change_minor, source_ref,
     reason_code, approval_id, created_at)
SELECT
     movement_id, business_day_id, session_id, register_id, operator_id, movement_type,
     direction, amount_minor, currency, tendered_minor, change_minor, source_ref,
     reason_code, approval_id, created_at
FROM cash_movements_v6;

DROP TABLE cash_movements_v6;

CREATE UNIQUE INDEX ux_cash_movements_source
    ON cash_movements (session_id, movement_type, source_ref)
    WHERE source_ref IS NOT NULL;

CREATE UNIQUE INDEX ux_cash_movements_opening_float
    ON cash_movements (session_id)
    WHERE movement_type = 'OPENING_FLOAT';

CREATE INDEX ix_cash_movements_session ON cash_movements (session_id, created_at);
CREATE INDEX ix_cash_movements_day ON cash_movements (business_day_id, created_at);

CREATE TRIGGER trg_cash_movements_no_update
BEFORE UPDATE ON cash_movements
BEGIN
    SELECT RAISE(ABORT, 'cash movements are append-only');
END;

CREATE TRIGGER trg_cash_movements_no_delete
BEFORE DELETE ON cash_movements
BEGIN
    SELECT RAISE(ABORT, 'cash movements are append-only');
END;

-- Inventory: REFUND_RETURN, direction IN. Goods that came back with an
-- accepted refund, exactly once per refund line.
ALTER TABLE inventory_movements RENAME TO inventory_movements_v7;

CREATE TABLE inventory_movements (
    movement_id      TEXT    PRIMARY KEY,
    store_id         TEXT    NOT NULL,
    product_ref      TEXT    NOT NULL,
    movement_type    TEXT    NOT NULL CHECK (movement_type IN
                                 ('OPENING_BALANCE', 'SALE', 'MANUAL_IN', 'MANUAL_OUT',
                                  'REFUND_RETURN')),
    direction        TEXT    NOT NULL CHECK (direction IN ('IN', 'OUT')),
    quantity_thousandths INTEGER NOT NULL CHECK (quantity_thousandths > 0),
    source_ref       TEXT    NOT NULL,
    transaction_id   TEXT    REFERENCES retail_transactions (transaction_id),
    line_number      INTEGER CHECK (line_number IS NULL OR line_number > 0),
    business_day_id  TEXT    REFERENCES business_days (business_day_id),
    session_id       TEXT    REFERENCES register_sessions (session_id),
    reason_code      TEXT,
    occurred_at      TEXT    NOT NULL,
    CHECK (
        CASE movement_type
            WHEN 'OPENING_BALANCE' THEN direction = 'IN'
            WHEN 'SALE'            THEN direction = 'OUT'
            WHEN 'MANUAL_IN'       THEN direction = 'IN'
            WHEN 'MANUAL_OUT'      THEN direction = 'OUT'
            WHEN 'REFUND_RETURN'   THEN direction = 'IN'
        END
    ),
    CHECK ((movement_type = 'SALE') = (transaction_id IS NOT NULL)),
    CHECK ((transaction_id IS NULL) = (line_number IS NULL)),
    CHECK (movement_type NOT IN ('MANUAL_IN', 'MANUAL_OUT')
           OR (reason_code IS NOT NULL AND TRIM(reason_code) <> ''))
);

INSERT INTO inventory_movements
    (movement_id, store_id, product_ref, movement_type, direction, quantity_thousandths,
     source_ref, transaction_id, line_number, business_day_id, session_id, reason_code,
     occurred_at)
SELECT
     movement_id, store_id, product_ref, movement_type, direction, quantity_thousandths,
     source_ref, transaction_id, line_number, business_day_id, session_id, reason_code,
     occurred_at
FROM inventory_movements_v7;

DROP TABLE inventory_movements_v7;

CREATE UNIQUE INDEX ux_inventory_movements_source
    ON inventory_movements (store_id, movement_type, product_ref, source_ref);

CREATE INDEX ix_inventory_movements_product
    ON inventory_movements (store_id, product_ref, occurred_at);

CREATE INDEX ix_inventory_movements_transaction
    ON inventory_movements (transaction_id)
    WHERE transaction_id IS NOT NULL;

CREATE TRIGGER trg_inventory_movements_no_update
BEFORE UPDATE ON inventory_movements
BEGIN
    SELECT RAISE(ABORT, 'inventory movements are append-only');
END;

CREATE TRIGGER trg_inventory_movements_no_delete
BEFORE DELETE ON inventory_movements
BEGIN
    SELECT RAISE(ABORT, 'inventory movements are append-only');
END;
