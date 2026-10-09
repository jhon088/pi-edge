-- PI.Edge local schema, version 7: the inventory ledger.
--
-- No stock column anywhere. `catalog_products` stays a projection of the
-- product master and gains nothing, because a mutable quantity cannot answer
-- when it changed, why, or because of what (ADR 026). The ledger answers all
-- three, and `on_hand` is derived from it.

-- The append-only movement ledger.
CREATE TABLE inventory_movements (
    movement_id      TEXT    PRIMARY KEY,
    store_id         TEXT    NOT NULL,
    -- The canonical product reference. Inventory keeps no product list of its
    -- own, so this is validated against the active catalog snapshot before a
    -- movement is accepted; there is deliberately no foreign key, because a
    -- movement must outlive the snapshot that validated it.
    product_ref      TEXT    NOT NULL,
    movement_type    TEXT    NOT NULL CHECK (movement_type IN
                                 ('OPENING_BALANCE', 'SALE', 'MANUAL_IN', 'MANUAL_OUT')),
    -- Explicit, never the sign of the quantity: quantities are magnitudes
    -- everywhere in PI.Edge.
    direction        TEXT    NOT NULL CHECK (direction IN ('IN', 'OUT')),
    -- Thousandths, the same representation as every other quantity in the
    -- core, so 0.750 kg is 750 here exactly as it is on a sale line.
    quantity_thousandths INTEGER NOT NULL CHECK (quantity_thousandths > 0),
    -- What caused it, and the idempotency key: a bootstrap snapshot id, a
    -- `transaction:line`, or the request that asked for a manual movement.
    source_ref       TEXT    NOT NULL,
    -- Set exactly for a sale, so a movement is traceable to its document.
    transaction_id   TEXT    REFERENCES retail_transactions (transaction_id),
    line_number      INTEGER CHECK (line_number IS NULL OR line_number > 0),
    -- Operational context, when the movement happened during trading.
    business_day_id  TEXT    REFERENCES business_days (business_day_id),
    session_id       TEXT    REFERENCES register_sessions (session_id),
    reason_code      TEXT,
    occurred_at      TEXT    NOT NULL,
    -- Each type moves exactly one way.
    CHECK (
        CASE movement_type
            WHEN 'OPENING_BALANCE' THEN direction = 'IN'
            WHEN 'SALE'            THEN direction = 'OUT'
            WHEN 'MANUAL_IN'       THEN direction = 'IN'
            WHEN 'MANUAL_OUT'      THEN direction = 'OUT'
        END
    ),
    -- A sale movement comes from a sale line, and only a sale movement does.
    CHECK ((movement_type = 'SALE') = (transaction_id IS NOT NULL)),
    CHECK ((transaction_id IS NULL) = (line_number IS NULL)),
    -- An operator-initiated movement has to say why.
    CHECK (movement_type NOT IN ('MANUAL_IN', 'MANUAL_OUT')
           OR (reason_code IS NOT NULL AND TRIM(reason_code) <> ''))
);

-- One movement per cause *per product*. A replayed finalization cannot write a
-- second movement for a line, and a replayed manual command cannot write a
-- second movement at all (ADR 027).
--
-- The product belongs in the key because one cause can legitimately move
-- several products: an opening position is a single snapshot that moves every
-- product in it, exactly once each. For a sale line or a manual command the
-- product is already implied by the cause, so including it loses nothing.
CREATE UNIQUE INDEX ux_inventory_movements_source
    ON inventory_movements (store_id, movement_type, product_ref, source_ref);

-- The hot path: every movement of one product in one store.
CREATE INDEX ix_inventory_movements_product
    ON inventory_movements (store_id, product_ref, occurred_at);

CREATE INDEX ix_inventory_movements_transaction
    ON inventory_movements (transaction_id)
    WHERE transaction_id IS NOT NULL;

-- The ledger is append-only. The domain refuses to edit a movement and the
-- adapter has no statement that could; these make it impossible rather than
-- merely unimplemented. A correction is a new compensating movement.
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

-- Which upstream baseline this ledger started from. One row, ever: the
-- database belongs to one Edge installation, so a store-scoped primary key is
-- one baseline per installation. A later baseline is a reconciliation, and
-- half a reconciliation is worse than none (ADR 028).
CREATE TABLE inventory_bootstrap_state (
    store_id           TEXT    PRIMARY KEY,
    source_snapshot_id TEXT    NOT NULL,
    generated_at       TEXT    NOT NULL,
    item_count         INTEGER NOT NULL CHECK (item_count > 0),
    bootstrapped_at    TEXT    NOT NULL
);

-- A bootstrap is a fact about provisioning, not working state.
CREATE TRIGGER trg_inventory_bootstrap_no_update
BEFORE UPDATE ON inventory_bootstrap_state
BEGIN
    SELECT RAISE(ABORT, 'the inventory bootstrap is recorded once');
END;

-- Whether this installation may sell what it does not have on record.
--
-- Off means availability is enforced before any money moves; it is never
-- enforced after, because refusing to write down goods that already left the
-- counter makes the ledger wrong rather than the store right (ADR 029).
ALTER TABLE device_profile
    ADD COLUMN allow_negative_inventory INTEGER NOT NULL DEFAULT 1
        CHECK (allow_negative_inventory IN (0, 1));
