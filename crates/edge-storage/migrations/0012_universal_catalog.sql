-- PI.Edge local schema, version 12: universal catalog extensions (ADR 044).
--
-- The same core serves a retail counter, a retail kiosk, a restaurant counter
-- and a restaurant kiosk, so none of this is vertical-specific: a catalog says
-- what it offers, on which channel, in which language, with which picture, and
-- which questions a product asks before it can be sold. Nothing here is a
-- rules engine and nothing here prices anything — a chargeable option points
-- at a product and the existing price books price it.
--
-- Purely additive: every table is new and every column added to an existing
-- table has a default, so a Retail catalog that declares none of this imports,
-- commits, activates and sells exactly as before.

-- A way of selling. Opaque upstream reference, like a store or a price book:
-- the core never enumerates the verticals that exist.
CREATE TABLE catalog_channels (
    snapshot_id          TEXT NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                              ON DELETE CASCADE,
    channel_ref          TEXT NOT NULL,
    name_default         TEXT NOT NULL,
    -- Declared per channel as data, not assumed: a counter offers everything
    -- unless told otherwise, a kiosk may well offer nothing until curated.
    default_availability TEXT NOT NULL CHECK (default_availability IN ('OFFERED', 'WITHHELD')),
    PRIMARY KEY (snapshot_id, channel_ref)
);

-- Localized text for every field that has any, in rows rather than a blob, so
-- the snapshot stays queryable and never has to be held in memory.
--
-- The owner is polymorphic, so there is no foreign key to follow: the core
-- validates every owner at commit, and the whole table cascades away with its
-- snapshot. `locale` is the exact tag the producer sent; resolution is an exact
-- match or the default, never a guess at a prefix.
CREATE TABLE catalog_text_translations (
    snapshot_id TEXT NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                     ON DELETE CASCADE,
    owner_kind  TEXT NOT NULL CHECK (owner_kind IN
                     ('CHANNEL', 'CATEGORY', 'PRODUCT', 'SELECTION_GROUP', 'SELECTION_OPTION')),
    owner_ref   TEXT NOT NULL,
    field       TEXT NOT NULL CHECK (field IN ('NAME', 'DISPLAY_NAME', 'DESCRIPTION')),
    locale      TEXT NOT NULL,
    text        TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, owner_kind, owner_ref, field, locale)
);

-- A grouping a client can browse. Flat on purpose.
CREATE TABLE catalog_categories (
    snapshot_id        TEXT    NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                               ON DELETE CASCADE,
    category_ref       TEXT    NOT NULL,
    name_default       TEXT    NOT NULL,
    display_order      INTEGER NOT NULL,
    -- An image is a reference and a content hash, never bytes: SQLite is not
    -- an asset store. Fetching the bytes is not this milestone.
    image_content_hash TEXT,
    image_media_type   TEXT,
    image_byte_size    INTEGER,
    PRIMARY KEY (snapshot_id, category_ref),
    CHECK ((image_content_hash IS NULL) = (image_media_type IS NULL)),
    CHECK ((image_content_hash IS NULL) = (image_byte_size IS NULL)),
    CHECK (image_byte_size IS NULL OR image_byte_size > 0)
);

-- How one product is shown. Separate from catalog_products because what may be
-- sold and what a customer sees are different questions, and because a retail
-- catalog answers only the first one.
CREATE TABLE catalog_product_presentation (
    snapshot_id          TEXT    NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                                 ON DELETE CASCADE,
    product_ref          TEXT    NOT NULL,
    -- NULL for a product reachable by code or barcode but not by browsing.
    category_ref         TEXT,
    -- Shown instead of the commercial name. The commercial name stays the one
    -- on the receipt and in the ledger.
    display_name_default TEXT,
    description_default  TEXT,
    display_order        INTEGER NOT NULL,
    image_content_hash   TEXT,
    image_media_type     TEXT,
    image_byte_size      INTEGER,
    PRIMARY KEY (snapshot_id, product_ref),
    FOREIGN KEY (snapshot_id, product_ref)
        REFERENCES catalog_products (snapshot_id, product_ref) ON DELETE CASCADE,
    FOREIGN KEY (snapshot_id, category_ref)
        REFERENCES catalog_categories (snapshot_id, category_ref) ON DELETE CASCADE,
    CHECK ((image_content_hash IS NULL) = (image_media_type IS NULL)),
    CHECK ((image_content_hash IS NULL) = (image_byte_size IS NULL)),
    CHECK (image_byte_size IS NULL OR image_byte_size > 0)
);

CREATE INDEX ix_catalog_presentation_category
    ON catalog_product_presentation (snapshot_id, category_ref, display_order);

-- One question a product (or a chosen option) asks before it can be sold.
--
-- min_quantity and max_quantity count UNITS across the whole group, which is
-- what "choose one size", "up to five extras" and "two of the same extra" all
-- reduce to. min_quantity >= 1 is what "required" means; there is no separate
-- flag to contradict it.
CREATE TABLE catalog_selection_groups (
    snapshot_id       TEXT    NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                              ON DELETE CASCADE,
    group_id          TEXT    NOT NULL,
    -- A group hangs from a product, or from an option of another group: that
    -- is how `Drink -> Coca-Cola -> Size` is expressed, with no tree type and
    -- no rules engine.
    owner_kind        TEXT    NOT NULL CHECK (owner_kind IN ('PRODUCT', 'OPTION')),
    owner_product_ref TEXT,
    owner_option_id   TEXT,
    name_default      TEXT    NOT NULL,
    min_quantity      INTEGER NOT NULL CHECK (min_quantity >= 0),
    max_quantity      INTEGER NOT NULL CHECK (max_quantity >= 1),
    display_order     INTEGER NOT NULL,
    PRIMARY KEY (snapshot_id, group_id),
    CHECK ((owner_kind = 'PRODUCT') = (owner_product_ref IS NOT NULL)),
    CHECK ((owner_kind = 'OPTION') = (owner_option_id IS NOT NULL)),
    CHECK (max_quantity >= min_quantity),
    FOREIGN KEY (snapshot_id, owner_product_ref)
        REFERENCES catalog_products (snapshot_id, product_ref) ON DELETE CASCADE
    -- owner_option_id deliberately has no foreign key: options reference their
    -- group and would close the cycle. The core validates the owner chain and
    -- refuses cycles at commit, which is also where a cycle is detectable.
);

CREATE INDEX ix_catalog_selection_groups_owner
    ON catalog_selection_groups (snapshot_id, owner_kind, owner_product_ref, owner_option_id);

-- One answer inside a group.
--
-- component_product_ref is how an option costs money: it names a product the
-- existing price books already price. There is no price column here and no
-- second pricing engine. An option without a component is purely semantic and
-- free — "no onions" is not a product.
CREATE TABLE catalog_selection_options (
    snapshot_id          TEXT    NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                                 ON DELETE CASCADE,
    option_id            TEXT    NOT NULL,
    group_id             TEXT    NOT NULL,
    name_default         TEXT    NOT NULL,
    component_product_ref TEXT,
    max_quantity         INTEGER NOT NULL CHECK (max_quantity >= 1),
    default_quantity     INTEGER NOT NULL CHECK (default_quantity >= 0),
    display_order        INTEGER NOT NULL,
    image_content_hash   TEXT,
    image_media_type     TEXT,
    image_byte_size      INTEGER,
    PRIMARY KEY (snapshot_id, option_id),
    CHECK (default_quantity <= max_quantity),
    CHECK ((image_content_hash IS NULL) = (image_media_type IS NULL)),
    CHECK ((image_content_hash IS NULL) = (image_byte_size IS NULL)),
    CHECK (image_byte_size IS NULL OR image_byte_size > 0),
    FOREIGN KEY (snapshot_id, group_id)
        REFERENCES catalog_selection_groups (snapshot_id, group_id) ON DELETE CASCADE,
    FOREIGN KEY (snapshot_id, component_product_ref)
        REFERENCES catalog_products (snapshot_id, product_ref) ON DELETE CASCADE
);

CREATE INDEX ix_catalog_selection_options_group
    ON catalog_selection_options (snapshot_id, group_id, display_order);

-- Commercial availability: whether something is OFFERED on a channel. This is
-- not inventory. "There is no stock" is edge-inventory's answer and has its own
-- table; this one answers "we do not sell that here", which is true even with a
-- full warehouse.
CREATE TABLE catalog_channel_availability (
    snapshot_id   TEXT NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                       ON DELETE CASCADE,
    channel_ref   TEXT NOT NULL,
    target_kind   TEXT NOT NULL CHECK (target_kind IN
                       ('PRODUCT', 'CATEGORY', 'SELECTION_OPTION')),
    target_ref    TEXT NOT NULL,
    -- SNOOZED is "sold out until", the one state a store actually operates by
    -- hand. It carries its own deadline so nothing has to guess when it lifts.
    state         TEXT NOT NULL CHECK (state IN ('OFFERED', 'WITHHELD', 'SNOOZED')),
    snoozed_until TEXT,
    -- One answer per channel and target: a channel cannot say two things.
    PRIMARY KEY (snapshot_id, channel_ref, target_kind, target_ref),
    CHECK ((state = 'SNOOZED') = (snoozed_until IS NOT NULL)),
    FOREIGN KEY (snapshot_id, channel_ref)
        REFERENCES catalog_channels (snapshot_id, channel_ref) ON DELETE CASCADE
    -- target_ref is polymorphic, so it has no foreign key either; the core
    -- resolves every target against the snapshot at commit.
);

CREATE INDEX ix_catalog_availability_target
    ON catalog_channel_availability (snapshot_id, target_kind, target_ref);

-- Snapshot-level expectations now include the extensions. Defaulting to zero is
-- what keeps a Retail import, and an upstream that has never heard of any of
-- this, committing unchanged.
ALTER TABLE catalog_snapshots
    ADD COLUMN expected_channels INTEGER NOT NULL DEFAULT 0;
ALTER TABLE catalog_snapshots
    ADD COLUMN expected_categories INTEGER NOT NULL DEFAULT 0;
ALTER TABLE catalog_snapshots
    ADD COLUMN expected_presentation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE catalog_snapshots
    ADD COLUMN expected_selection_groups INTEGER NOT NULL DEFAULT 0;
ALTER TABLE catalog_snapshots
    ADD COLUMN expected_selection_options INTEGER NOT NULL DEFAULT 0;
ALTER TABLE catalog_snapshots
    ADD COLUMN expected_availability INTEGER NOT NULL DEFAULT 0;

-- Chunks may carry the bulk extensions, counted like products and promotions so
-- a commit can still prove that what arrived is what was declared.
ALTER TABLE catalog_import_chunks
    ADD COLUMN presentation_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE catalog_import_chunks
    ADD COLUMN selection_group_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE catalog_import_chunks
    ADD COLUMN selection_option_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE catalog_import_chunks
    ADD COLUMN availability_count INTEGER NOT NULL DEFAULT 0;
