-- PI.Edge local schema, version 4: promotions.
--
-- Promotions arrive in the snapshot like products and prices do (ADR 015). The
-- rule is stored in typed columns rather than as a JSON blob so SQLite can
-- check it: every rule kind declares exactly which slots it fills, and a CHECK
-- refuses the rest. A blob would make an invalid rule a runtime surprise.

CREATE TABLE promotions (
    snapshot_id    TEXT    NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                           ON DELETE CASCADE,
    -- Opaque reference assigned upstream.
    promotion_id   TEXT    NOT NULL,
    -- Short human code, unique within the snapshot. Shown on receipts.
    code           TEXT    NOT NULL,
    name           TEXT    NOT NULL,
    active         INTEGER NOT NULL CHECK (active IN (0, 1)),
    -- Higher wins on the same target. Ties between exclusives are refused at
    -- evaluation time, not here: two exclusives at one priority are only
    -- ambiguous when they actually meet in the same basket.
    priority       INTEGER NOT NULL,
    stackability   TEXT    NOT NULL CHECK (stackability IN ('STACKABLE', 'EXCLUSIVE')),
    scope          TEXT    NOT NULL CHECK (scope IN ('TENANT', 'STORE')),
    -- Set exactly when the scope is STORE.
    scope_store_id TEXT,
    -- Inclusive lower bound, exclusive upper bound, both optional.
    valid_from     TEXT,
    valid_until    TEXT,
    rule_kind      TEXT    NOT NULL CHECK (rule_kind IN
                               ('PERCENTAGE_OFF', 'FIXED_AMOUNT_OFF',
                                'BUY_N_GET_M', 'BASKET_THRESHOLD')),
    -- NULL means "any product"; line rules only.
    applies_to_product_ref TEXT,
    -- PERCENTAGE_OFF: basis points, never above 100%.
    percentage_bps INTEGER CHECK (percentage_bps IS NULL OR
                                  (percentage_bps > 0 AND percentage_bps <= 10000)),
    -- FIXED_AMOUNT_OFF and BASKET_THRESHOLD: what comes off.
    amount_minor   INTEGER CHECK (amount_minor IS NULL OR amount_minor > 0),
    -- BASKET_THRESHOLD: what the basket must reach first.
    minimum_minor  INTEGER CHECK (minimum_minor IS NULL OR minimum_minor > 0),
    -- Currency of amount_minor / minimum_minor.
    currency       TEXT,
    -- BUY_N_GET_M, in thousandths like every other quantity.
    buy_thousandths INTEGER CHECK (buy_thousandths IS NULL OR buy_thousandths > 0),
    get_thousandths INTEGER CHECK (get_thousandths IS NULL OR get_thousandths > 0),
    PRIMARY KEY (snapshot_id, promotion_id),
    -- A promotion that names a product must travel in the chunk that carries
    -- that product, exactly as a price entry does.
    FOREIGN KEY (snapshot_id, applies_to_product_ref)
        REFERENCES catalog_products (snapshot_id, product_ref) ON DELETE CASCADE,
    CHECK ((scope = 'STORE') = (scope_store_id IS NOT NULL)),
    CHECK (valid_until IS NULL OR valid_from IS NULL OR valid_until > valid_from),
    -- A money rule carries a currency and nothing else does.
    CHECK ((currency IS NOT NULL) = (amount_minor IS NOT NULL)),
    -- Each rule kind fills exactly its own slots.
    CHECK (
        CASE rule_kind
            WHEN 'PERCENTAGE_OFF' THEN
                percentage_bps IS NOT NULL AND amount_minor IS NULL
                AND minimum_minor IS NULL AND buy_thousandths IS NULL
                AND get_thousandths IS NULL
            WHEN 'FIXED_AMOUNT_OFF' THEN
                amount_minor IS NOT NULL AND percentage_bps IS NULL
                AND minimum_minor IS NULL AND buy_thousandths IS NULL
                AND get_thousandths IS NULL
            WHEN 'BUY_N_GET_M' THEN
                buy_thousandths IS NOT NULL AND get_thousandths IS NOT NULL
                AND percentage_bps IS NULL AND amount_minor IS NULL
                AND minimum_minor IS NULL
            WHEN 'BASKET_THRESHOLD' THEN
                minimum_minor IS NOT NULL AND amount_minor IS NOT NULL
                AND percentage_bps IS NULL AND buy_thousandths IS NULL
                AND get_thousandths IS NULL AND applies_to_product_ref IS NULL
        END
    )
);

CREATE UNIQUE INDEX ux_promotions_code ON promotions (snapshot_id, code);

-- The candidate query filters on exactly these columns.
CREATE INDEX ix_promotions_candidates
    ON promotions (snapshot_id, active, scope, scope_store_id);

-- Snapshot-level expectations now include promotions.
ALTER TABLE catalog_snapshots
    ADD COLUMN expected_promotions INTEGER NOT NULL DEFAULT 0;

-- Chunks may carry promotions.
ALTER TABLE catalog_import_chunks
    ADD COLUMN promotion_count INTEGER NOT NULL DEFAULT 0;

-- What a sale actually granted, materialised so the receipt and the audit
-- survive the purge of the snapshot that produced it. The promotion's code and
-- name are copied, not referenced: this row must still read correctly when the
-- promotion is gone from the catalog.
CREATE TABLE transaction_promotions (
    transaction_id  TEXT    NOT NULL REFERENCES retail_transactions (transaction_id)
                            ON DELETE CASCADE,
    -- Deterministic composite of promotion and target: re-evaluating the same
    -- basket produces the same id, which is what makes the write idempotent.
    adjustment_id   TEXT    NOT NULL,
    -- Order the engine applied them in, so a receipt can replay the sequence.
    sequence        INTEGER NOT NULL CHECK (sequence >= 0),
    promotion_id    TEXT    NOT NULL,
    promotion_code  TEXT    NOT NULL,
    promotion_name  TEXT    NOT NULL,
    priority        INTEGER NOT NULL,
    stackability    TEXT    NOT NULL CHECK (stackability IN ('STACKABLE', 'EXCLUSIVE')),
    -- 'LINE' or 'BASKET'; line_number is set exactly for LINE.
    target          TEXT    NOT NULL CHECK (target IN ('LINE', 'BASKET')),
    line_number     INTEGER CHECK (line_number IS NULL OR line_number > 0),
    amount_minor    INTEGER NOT NULL CHECK (amount_minor >= 0),
    currency        TEXT    NOT NULL,
    reason_code     TEXT    NOT NULL,
    detail          TEXT,
    created_at      TEXT    NOT NULL,
    PRIMARY KEY (transaction_id, adjustment_id),
    CHECK ((target = 'LINE') = (line_number IS NOT NULL))
);

CREATE INDEX ix_transaction_promotions_sequence
    ON transaction_promotions (transaction_id, sequence);

-- Lines can now be removed, so the next line number cannot be derived from the
-- lines that remain: a removed number is never reused, and the counter has to
-- survive a reload. (The discounted total is not stored again here — it is
-- already `discount_total_minor`.)
ALTER TABLE retail_transactions ADD COLUMN next_line_number INTEGER NOT NULL DEFAULT 1;
