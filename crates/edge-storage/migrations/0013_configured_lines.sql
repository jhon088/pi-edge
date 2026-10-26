-- PI.Edge local schema, version 13: a sale line can be a configured product
-- (ADR 046).
--
-- Purely additive. Two defaulted columns on `transaction_lines` and one child
-- table, so every retail line already stored keeps its exact meaning: with no
-- selections, `base_unit_price_minor` *is* the unit price.
--
-- The selections are **materialised**, not referenced: the option's name, its
-- price and its amount are copied onto the sale, exactly as a promotion's code
-- and name are (ADR 015). This row has to still read correctly on a receipt or
-- a refund years after the snapshot that offered the option is gone.

-- What the product cost before any choice was made. Defaulting to zero would
-- lie about existing rows, so the rebuild below backfills it from the unit
-- price that was actually charged.
ALTER TABLE transaction_lines
    ADD COLUMN base_unit_price_minor INTEGER NOT NULL DEFAULT 0;

-- Free text for whoever prepares it. Never priced, never taxed, never part of
-- any total.
ALTER TABLE transaction_lines ADD COLUMN preparation_note TEXT;

-- Every line that existed before configurations did was unconfigured, and an
-- unconfigured line's base price is the price it charged.
UPDATE transaction_lines SET base_unit_price_minor = unit_price_minor;

CREATE TABLE transaction_line_selections (
    line_id        TEXT    NOT NULL REFERENCES transaction_lines (line_id)
                           ON DELETE CASCADE,
    -- Order the choices were resolved in, so a receipt replays them the way
    -- the customer made them.
    sequence       INTEGER NOT NULL CHECK (sequence >= 0),
    -- Upstream references, kept for traceability. Deliberately *not* foreign
    -- keys to the catalog: the snapshot may be purged and this row must
    -- survive it.
    option_id      TEXT    NOT NULL,
    group_id       TEXT    NOT NULL,
    -- Copied, not joined: what the customer actually read on the screen.
    group_name     TEXT    NOT NULL,
    name           TEXT    NOT NULL,
    quantity       INTEGER NOT NULL CHECK (quantity >= 1),
    -- The product a chargeable choice billed through. NULL is a purely
    -- semantic choice — "no onions" is not a product (ADR 044).
    component_ref  TEXT,
    unit_price_minor INTEGER NOT NULL CHECK (unit_price_minor >= 0),
    amount_minor   INTEGER NOT NULL CHECK (amount_minor >= 0),
    currency       TEXT    NOT NULL,
    created_at     TEXT    NOT NULL,
    PRIMARY KEY (line_id, option_id),
    -- A semantic choice costs nothing, and only a chargeable one can.
    CHECK ((component_ref IS NULL) = (unit_price_minor = 0 AND amount_minor = 0)
           OR component_ref IS NOT NULL)
);

CREATE INDEX ix_transaction_line_selections_sequence
    ON transaction_line_selections (line_id, sequence);
