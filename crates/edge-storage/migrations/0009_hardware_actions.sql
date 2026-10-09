-- PI.Edge local schema, version 9: the hardware action log.
--
-- One table, and a narrow purpose: stop an automatic physical side effect from
-- happening twice, and leave an audit trail of what the hardware did
-- (ADR 037).
--
-- It is **not** a print queue, not a job scheduler and not a receipt. The
-- source of a reprint is the sale, never a row here, and the commercial and
-- fiscal lifecycle of a receipt belongs to its own milestone (ADR 039).

CREATE TABLE hardware_actions (
    action_id     TEXT    PRIMARY KEY,
    -- What was attempted.
    kind          TEXT    NOT NULL CHECK (kind IN ('DRAWER_OPEN', 'PRINT')),
    device_kind   TEXT    NOT NULL CHECK (device_kind IN ('PRINTER', 'CASH_DRAWER', 'SCANNER')),
    -- Why: a `DrawerOpenReason` or a `PrintKind`. There is no generic "open
    -- it", so this always names a commercial cause or an explicit diagnostic.
    reason        TEXT    NOT NULL,
    -- The commercial fact that justified it: a payment id for a cash sale, a
    -- refund payment id for a cash refund. NULL for an operator's explicit
    -- diagnostic request, which may legitimately repeat.
    source_ref    TEXT,
    -- PENDING is written *before* the device is touched, so a process that
    -- dies mid-action leaves evidence that it may already have happened
    -- (ADR 020, applied to hardware).
    status        TEXT    NOT NULL CHECK (status IN ('PENDING', 'COMPLETED', 'FAILED')),
    error_code    TEXT,
    -- Human detail for diagnosis. Never a stack trace, never a payload, never
    -- a host or a port.
    error_message TEXT,
    requested_at  TEXT    NOT NULL,
    completed_at  TEXT,
    -- A resolved action says when it resolved; a pending one cannot.
    CHECK ((status IN ('COMPLETED', 'FAILED')) = (completed_at IS NOT NULL)),
    -- Only a failure explains itself.
    CHECK ((status = 'FAILED') = (error_code IS NOT NULL))
);

-- The whole point of the table: one automatic physical action per cause.
--
-- `PENDING` counts as taken. If a process died after opening the drawer and
-- before recording it, we do not know whether the money is exposed — and the
-- safe answer to "did it already open?" is yes. An operator can always open it
-- again from the diagnostic surface, which is deliberate and audited; what must
-- never happen is a drawer opening twice because a command was replayed.
--
-- A `FAILED` action is *not* in the index, so a cause whose device was down can
-- be attempted again.
CREATE UNIQUE INDEX ux_hardware_actions_source
    ON hardware_actions (kind, source_ref)
    WHERE source_ref IS NOT NULL AND status IN ('PENDING', 'COMPLETED');

-- The diagnostic read: what did this device last do, and when did it last work.
CREATE INDEX ix_hardware_actions_device
    ON hardware_actions (device_kind, requested_at);

-- An action is evidence of a physical event. A correction is a new action.
CREATE TRIGGER trg_hardware_actions_no_delete
BEFORE DELETE ON hardware_actions
BEGIN
    SELECT RAISE(ABORT, 'hardware actions are append-only');
END;
