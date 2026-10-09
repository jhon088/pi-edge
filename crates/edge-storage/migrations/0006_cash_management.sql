-- PI.Edge local schema, version 6: cash management.
--
-- Three new tables and a handful of session/day columns. The ledger is the
-- source of truth for expected cash (ADR 021); nothing stores a mutable
-- expected figure, and the only materialised one lives on a close declaration
-- so a historical variance cannot be recomputed under future rules (ADR 022).

-- The append-only drawer ledger.
CREATE TABLE cash_movements (
    movement_id     TEXT    PRIMARY KEY,
    business_day_id TEXT    NOT NULL REFERENCES business_days (business_day_id),
    session_id      TEXT    NOT NULL REFERENCES register_sessions (session_id),
    register_id     TEXT    NOT NULL,
    -- Who caused it. Identity belongs to the action, not to the register.
    operator_id     TEXT    NOT NULL,
    movement_type   TEXT    NOT NULL CHECK (movement_type IN
                                ('OPENING_FLOAT', 'CASH_SALE', 'CASH_IN', 'CASH_OUT',
                                 'SAFE_DROP')),
    -- Explicit, never the sign of the amount: amounts stay non-negative and
    -- reading a drawer's history must not depend on spotting a minus sign.
    direction       TEXT    NOT NULL CHECK (direction IN ('IN', 'OUT')),
    amount_minor    INTEGER NOT NULL CHECK (amount_minor > 0),
    currency        TEXT    NOT NULL,
    -- CASH_SALE only: what the customer handed over and what went back, so the
    -- tender/change audit sits on the same line as the net that stayed.
    tendered_minor  INTEGER CHECK (tendered_minor IS NULL OR tendered_minor > 0),
    change_minor    INTEGER CHECK (change_minor IS NULL OR change_minor >= 0),
    -- What caused it, when something else owns that fact: a payment_id for a
    -- sale, a safe_drop_id for a drop. Also the idempotency key.
    source_ref      TEXT,
    reason_code     TEXT,
    approval_id     TEXT    REFERENCES approvals (approval_id),
    created_at      TEXT    NOT NULL,
    -- Each type moves exactly one way.
    CHECK (
        CASE movement_type
            WHEN 'OPENING_FLOAT' THEN direction = 'IN'
            WHEN 'CASH_SALE'     THEN direction = 'IN'
            WHEN 'CASH_IN'       THEN direction = 'IN'
            WHEN 'CASH_OUT'      THEN direction = 'OUT'
            WHEN 'SAFE_DROP'     THEN direction = 'OUT'
        END
    ),
    -- Only a cash sale carries the tender audit, and it has to balance:
    -- amount = tendered - change.
    CHECK ((movement_type = 'CASH_SALE') = (tendered_minor IS NOT NULL)),
    CHECK ((tendered_minor IS NULL) = (change_minor IS NULL)),
    CHECK (tendered_minor IS NULL OR amount_minor = tendered_minor - change_minor),
    -- An operator-initiated movement has to say why.
    CHECK (movement_type NOT IN ('CASH_IN', 'CASH_OUT')
           OR (reason_code IS NOT NULL AND TRIM(reason_code) <> ''))
);

-- One movement per cause: a replayed payment or a re-approved drop cannot
-- move the drawer twice.
CREATE UNIQUE INDEX ux_cash_movements_source
    ON cash_movements (session_id, movement_type, source_ref)
    WHERE source_ref IS NOT NULL;

-- One opening float per session.
CREATE UNIQUE INDEX ux_cash_movements_opening_float
    ON cash_movements (session_id)
    WHERE movement_type = 'OPENING_FLOAT';

CREATE INDEX ix_cash_movements_session ON cash_movements (session_id, created_at);
CREATE INDEX ix_cash_movements_day ON cash_movements (business_day_id, created_at);

-- The ledger is append-only. The domain refuses to edit a movement and the
-- adapter has no statement that could; these make it impossible rather than
-- merely unimplemented. A correction is a new compensating movement.
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

-- Who asked, who agreed. Deliberately not an authorisation system: the
-- application layer decides, this records (ADR 023).
CREATE TABLE approvals (
    approval_id   TEXT    PRIMARY KEY,
    action_type   TEXT    NOT NULL CHECK (action_type IN ('SAFE_DROP')),
    -- What is being approved, by its own id.
    aggregate_ref TEXT    NOT NULL,
    requested_by  TEXT    NOT NULL,
    requested_at  TEXT    NOT NULL,
    approved_by   TEXT,
    decided_at    TEXT,
    status        TEXT    NOT NULL CHECK (status IN ('PENDING', 'APPROVED', 'REJECTED')),
    reason        TEXT,
    -- A decision has a decider and an instant, and a pending one has neither.
    CHECK ((status = 'PENDING') = (approved_by IS NULL)),
    CHECK ((approved_by IS NULL) = (decided_at IS NULL)),
    -- V1 policy: nobody approves their own request.
    CHECK (approved_by IS NULL OR approved_by <> requested_by)
);

CREATE INDEX ix_approvals_pending ON approvals (status, action_type);

CREATE TABLE safe_drops (
    safe_drop_id TEXT    PRIMARY KEY,
    session_id   TEXT    NOT NULL REFERENCES register_sessions (session_id),
    approval_id  TEXT    NOT NULL UNIQUE REFERENCES approvals (approval_id),
    amount_minor INTEGER NOT NULL CHECK (amount_minor > 0),
    currency     TEXT    NOT NULL,
    reason_code  TEXT    NOT NULL CHECK (TRIM(reason_code) <> ''),
    status       TEXT    NOT NULL CHECK (status IN
                             ('PENDING_APPROVAL', 'APPROVED', 'REJECTED')),
    requested_by TEXT    NOT NULL,
    requested_at TEXT    NOT NULL,
    approved_by  TEXT,
    approved_at  TEXT,
    CHECK ((status = 'PENDING_APPROVAL') = (approved_by IS NULL)),
    CHECK ((approved_by IS NULL) = (approved_at IS NULL)),
    CHECK (approved_by IS NULL OR approved_by <> requested_by)
);

CREATE INDEX ix_safe_drops_session ON safe_drops (session_id, status);

-- What an operator counted, against what the ledger said at that instant.
CREATE TABLE session_close_declarations (
    declaration_id    TEXT    PRIMARY KEY,
    session_id        TEXT    NOT NULL REFERENCES register_sessions (session_id),
    closure_kind      TEXT    NOT NULL CHECK (closure_kind IN ('DECLARED', 'FORCED')),
    -- NULL for a forced close: no count was taken, and inventing one would
    -- manufacture a clean variance out of nothing (ADR 024).
    declared_minor    INTEGER CHECK (declared_minor IS NULL OR declared_minor >= 0),
    -- The ledger figure used at this close. Materialised on purpose.
    expected_minor    INTEGER NOT NULL,
    -- declared - expected. NULL exactly when the count is.
    variance_minor    INTEGER,
    currency          TEXT    NOT NULL,
    declared_by       TEXT,
    declared_at       TEXT    NOT NULL,
    reason            TEXT,
    -- A declared close has a count, a counter and a variance; a forced one has
    -- none of those and a reason instead.
    CHECK ((closure_kind = 'DECLARED') = (declared_minor IS NOT NULL)),
    CHECK ((declared_minor IS NULL) = (variance_minor IS NULL)),
    CHECK ((declared_minor IS NULL) = (declared_by IS NULL)),
    CHECK (variance_minor IS NULL OR variance_minor = declared_minor - expected_minor),
    CHECK (closure_kind <> 'FORCED' OR (reason IS NOT NULL AND TRIM(reason) <> ''))
);

-- One declaration per session close. A corrected count is a new close after a
-- reopen, which leaves the first declaration visible forever (ADR 023).
CREATE UNIQUE INDEX ux_close_declarations_session
    ON session_close_declarations (session_id);

-- Declarations are evidence, not working state.
CREATE TRIGGER trg_close_declarations_no_update
BEFORE UPDATE ON session_close_declarations
BEGIN
    SELECT RAISE(ABORT, 'close declarations are immutable');
END;

-- How a session ended, and what it continues.
ALTER TABLE register_sessions ADD COLUMN closure_kind TEXT
    CHECK (closure_kind IS NULL OR closure_kind IN ('DECLARED', 'FORCED'));
ALTER TABLE register_sessions ADD COLUMN close_declaration_id TEXT
    REFERENCES session_close_declarations (declaration_id);
-- A reopen creates a new session linked to the closed one; the earlier
-- session is never mutated (ADR 023).
ALTER TABLE register_sessions ADD COLUMN reopened_from_session_id TEXT
    REFERENCES register_sessions (session_id);

-- How the day ended, why, and on whose authority.
ALTER TABLE business_days ADD COLUMN closure_kind TEXT
    CHECK (closure_kind IS NULL OR closure_kind IN ('DECLARED', 'FORCED'));
ALTER TABLE business_days ADD COLUMN close_reason TEXT;
ALTER TABLE business_days ADD COLUMN closed_by TEXT;
