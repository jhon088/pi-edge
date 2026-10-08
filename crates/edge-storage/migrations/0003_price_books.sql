-- PI.Edge local schema, version 3: pricing leaves the product.
--
-- A product states what it is; a price book states what it costs in a context.
-- Separating them is what makes a store override and a scheduled price change
-- possible without touching product identity (ADR 014).
--
-- Two columns have to disappear, and both carry CHECK constraints, which SQLite
-- will not let `ALTER TABLE ... DROP COLUMN` remove. So those two tables are
-- rebuilt. `defer_foreign_keys` is used rather than `foreign_keys = OFF`
-- because the latter is a no-op inside a transaction, and migrations run in one.
PRAGMA defer_foreign_keys = ON;

CREATE TABLE price_books (
    snapshot_id    TEXT    NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                           ON DELETE CASCADE,
    -- Opaque reference assigned upstream.
    price_book_id  TEXT    NOT NULL,
    -- Short human code, unique within the snapshot. Diagnostics only.
    code           TEXT    NOT NULL,
    name           TEXT    NOT NULL,
    currency       TEXT    NOT NULL,
    scope          TEXT    NOT NULL CHECK (scope IN ('TENANT', 'STORE')),
    -- Set exactly when the scope is STORE.
    scope_store_id TEXT,
    -- Tiebreaker within a scope: higher wins.
    priority       INTEGER NOT NULL,
    active         INTEGER NOT NULL CHECK (active IN (0, 1)),
    PRIMARY KEY (snapshot_id, price_book_id),
    CHECK ((scope = 'STORE') = (scope_store_id IS NOT NULL))
);

CREATE UNIQUE INDEX ux_price_books_code ON price_books (snapshot_id, code);

-- The candidate query filters on exactly these columns.
CREATE INDEX ix_price_books_applicable
    ON price_books (snapshot_id, active, scope, scope_store_id);

CREATE TABLE price_entries (
    -- Surrogate key: the natural key includes a nullable validity bound, and
    -- SQLite does not enforce uniqueness over NULLs in a primary key.
    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_id   TEXT    NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                          ON DELETE CASCADE,
    price_book_id TEXT    NOT NULL,
    product_ref   TEXT    NOT NULL,
    amount_minor  INTEGER NOT NULL CHECK (amount_minor >= 0),
    currency      TEXT    NOT NULL,
    -- Whether the amount already includes tax: a property of the price.
    price_mode    TEXT    NOT NULL CHECK (price_mode IN ('TAX_EXCLUSIVE', 'TAX_INCLUSIVE')),
    -- Inclusive lower bound, exclusive upper bound, both optional.
    valid_from    TEXT,
    valid_until   TEXT,
    FOREIGN KEY (snapshot_id, price_book_id)
        REFERENCES price_books (snapshot_id, price_book_id) ON DELETE CASCADE,
    FOREIGN KEY (snapshot_id, product_ref)
        REFERENCES catalog_products (snapshot_id, product_ref) ON DELETE CASCADE,
    CHECK (valid_until IS NULL OR valid_from IS NULL OR valid_until > valid_from)
);

-- One price per (book, product, start instant). Overlapping windows are still
-- possible and are reported as AMBIGUOUS_PRICE when they actually collide.
CREATE UNIQUE INDEX ux_price_entries_identity
    ON price_entries (snapshot_id, price_book_id, product_ref, IFNULL(valid_from, ''));

-- The hot path: every candidate for one product in one snapshot.
CREATE INDEX ix_price_entries_lookup
    ON price_entries (snapshot_id, product_ref, price_book_id);

CREATE INDEX ix_price_entries_validity
    ON price_entries (snapshot_id, product_ref, valid_from, valid_until);

-- A tax profile states a rate. Whether an amount includes it is the price's
-- business, so price_mode moves to price_entries.
CREATE TABLE catalog_tax_profiles_v3 (
    snapshot_id TEXT    NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                        ON DELETE CASCADE,
    code        TEXT    NOT NULL,
    name        TEXT    NOT NULL,
    rate_bps    INTEGER NOT NULL CHECK (rate_bps >= 0),
    PRIMARY KEY (snapshot_id, code)
);

INSERT INTO catalog_tax_profiles_v3 (snapshot_id, code, name, rate_bps)
SELECT snapshot_id, code, name, rate_bps FROM catalog_tax_profiles;

DROP TABLE catalog_tax_profiles;
ALTER TABLE catalog_tax_profiles_v3 RENAME TO catalog_tax_profiles;

-- A product no longer owns a price.
--
-- Snapshots imported before this migration keep their products and barcodes but
-- have no price books, so every scan against them answers PRICE_NOT_FOUND until
-- the catalog is re-imported. That is a loud, stable failure rather than a
-- silent wrong price, and commit-time validation guarantees every *new*
-- snapshot carries pricing.
CREATE TABLE catalog_products_v3 (
    snapshot_id      TEXT    NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                             ON DELETE CASCADE,
    product_ref      TEXT    NOT NULL,
    external_ref     TEXT,
    sku              TEXT,
    name             TEXT    NOT NULL,
    active           INTEGER NOT NULL CHECK (active IN (0, 1)),
    sellable         INTEGER NOT NULL CHECK (sellable IN (0, 1)),
    tax_profile_code TEXT    NOT NULL,
    PRIMARY KEY (snapshot_id, product_ref),
    FOREIGN KEY (snapshot_id, tax_profile_code)
        REFERENCES catalog_tax_profiles (snapshot_id, code)
);

INSERT INTO catalog_products_v3
    (snapshot_id, product_ref, external_ref, sku, name, active, sellable, tax_profile_code)
SELECT snapshot_id, product_ref, external_ref, sku, name, active, sellable, tax_profile_code
FROM catalog_products;

DROP TABLE catalog_products;
ALTER TABLE catalog_products_v3 RENAME TO catalog_products;

CREATE INDEX ix_catalog_products_sku ON catalog_products (snapshot_id, sku);

-- Snapshot-level expectations now include pricing.
ALTER TABLE catalog_snapshots
    ADD COLUMN expected_price_books INTEGER NOT NULL DEFAULT 0;
ALTER TABLE catalog_snapshots
    ADD COLUMN expected_price_entries INTEGER NOT NULL DEFAULT 0;

-- Chunks carry price entries too.
ALTER TABLE catalog_import_chunks
    ADD COLUMN price_entry_count INTEGER NOT NULL DEFAULT 0;

-- A sale pins the instant it prices at, not only the snapshot.
ALTER TABLE retail_transactions ADD COLUMN pricing_effective_at TEXT;

-- The book that produced each line's price, for audit.
ALTER TABLE transaction_lines ADD COLUMN price_book_id TEXT;
