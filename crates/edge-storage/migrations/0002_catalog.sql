-- PI.Edge local schema, version 2: the local projection of the product master.
--
-- Products are never created here. Each row belongs to exactly one immutable
-- snapshot produced upstream, and every table cascades from the snapshot so an
-- aborted import can be discarded in one statement.

CREATE TABLE catalog_snapshots (
    snapshot_id           TEXT    PRIMARY KEY,
    tenant_id             TEXT    NOT NULL,
    store_id              TEXT    NOT NULL,
    -- Monotonic version assigned upstream; the operational identity of the
    -- content, together with content_hash.
    version               INTEGER NOT NULL CHECK (version > 0),
    status                TEXT    NOT NULL CHECK (status IN
                              ('IMPORTING', 'PENDING', 'ACTIVE', 'SUPERSEDED')),
    -- What the client said the content hashes to.
    declared_content_hash TEXT    NOT NULL,
    -- What this core computed from the rows it actually stored.
    content_hash          TEXT,
    generated_at          TEXT    NOT NULL,
    effective_at          TEXT,
    expected_tax_profiles INTEGER NOT NULL CHECK (expected_tax_profiles >= 0),
    expected_products     INTEGER NOT NULL CHECK (expected_products >= 0),
    expected_barcodes     INTEGER NOT NULL CHECK (expected_barcodes >= 0),
    imported_at           TEXT    NOT NULL,
    committed_at          TEXT,
    activated_at          TEXT,
    superseded_at         TEXT,
    created_at            TEXT    NOT NULL,
    updated_at            TEXT    NOT NULL,
    -- A version is the upstream identity of one content set.
    UNIQUE (store_id, version),
    -- Only a committed snapshot has a computed hash.
    CHECK ((status = 'IMPORTING') = (content_hash IS NULL))
);

-- Invariant: at most one ACTIVE snapshot per store, so activation can never be
-- observed as "two active" and the lookup path never has to choose.
CREATE UNIQUE INDEX ux_catalog_snapshots_active
    ON catalog_snapshots (store_id)
    WHERE status = 'ACTIVE';

CREATE INDEX ix_catalog_snapshots_status
    ON catalog_snapshots (store_id, status, version DESC);

-- Durable record of every accepted chunk: what makes a chunk replay idempotent
-- and lets a commit prove no chunk is missing.
CREATE TABLE catalog_import_chunks (
    snapshot_id   TEXT    NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                          ON DELETE CASCADE,
    chunk_index   INTEGER NOT NULL CHECK (chunk_index >= 0),
    chunk_hash    TEXT    NOT NULL,
    product_count INTEGER NOT NULL CHECK (product_count >= 0),
    barcode_count INTEGER NOT NULL CHECK (barcode_count >= 0),
    received_at   TEXT    NOT NULL,
    PRIMARY KEY (snapshot_id, chunk_index)
);

CREATE TABLE catalog_tax_profiles (
    snapshot_id TEXT    NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                        ON DELETE CASCADE,
    code        TEXT    NOT NULL,
    name        TEXT    NOT NULL,
    rate_bps    INTEGER NOT NULL CHECK (rate_bps >= 0),
    -- Declared by the catalog, never inferred from the amount or the product.
    price_mode  TEXT    NOT NULL CHECK (price_mode IN ('TAX_EXCLUSIVE', 'TAX_INCLUSIVE')),
    PRIMARY KEY (snapshot_id, code)
);

CREATE TABLE catalog_products (
    snapshot_id      TEXT    NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                             ON DELETE CASCADE,
    -- Canonical identity, assigned upstream.
    product_ref      TEXT    NOT NULL,
    -- Source-system code: traceability metadata, never operational identity.
    external_ref     TEXT,
    sku              TEXT,
    name             TEXT    NOT NULL,
    active           INTEGER NOT NULL CHECK (active IN (0, 1)),
    sellable         INTEGER NOT NULL CHECK (sellable IN (0, 1)),
    tax_profile_code TEXT    NOT NULL,
    unit_price_minor INTEGER NOT NULL CHECK (unit_price_minor >= 0),
    currency         TEXT    NOT NULL,
    PRIMARY KEY (snapshot_id, product_ref),
    FOREIGN KEY (snapshot_id, tax_profile_code)
        REFERENCES catalog_tax_profiles (snapshot_id, code)
);

CREATE INDEX ix_catalog_products_sku ON catalog_products (snapshot_id, sku);

CREATE TABLE catalog_barcodes (
    snapshot_id TEXT NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                     ON DELETE CASCADE,
    -- The primary key is the barcode lookup index and the duplicate guard.
    barcode     TEXT NOT NULL,
    product_ref TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, barcode),
    FOREIGN KEY (snapshot_id, product_ref)
        REFERENCES catalog_products (snapshot_id, product_ref)
        ON DELETE CASCADE
);

CREATE INDEX ix_catalog_barcodes_product ON catalog_barcodes (snapshot_id, product_ref);

-- A sale records which snapshot priced it, and keeps resolving against that
-- one for its whole life (ADR 013).
ALTER TABLE retail_transactions
    ADD COLUMN catalog_snapshot_id TEXT REFERENCES catalog_snapshots (snapshot_id);
ALTER TABLE retail_transactions ADD COLUMN catalog_version INTEGER;

-- Net (tax base) total, so a receipt can print subtotal + tax = total in both
-- price modes.
ALTER TABLE retail_transactions ADD COLUMN net_total_minor INTEGER NOT NULL DEFAULT 0;

-- A line's quoted amount is no longer necessarily net: rename for accuracy and
-- record the mode that priced it.
ALTER TABLE transaction_lines RENAME COLUMN gross_amount_minor TO extended_amount_minor;
ALTER TABLE transaction_lines ADD COLUMN price_mode TEXT NOT NULL DEFAULT 'TAX_EXCLUSIVE';
