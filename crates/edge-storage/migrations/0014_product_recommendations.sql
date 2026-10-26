-- What to offer alongside a product.
--
-- Merchandising, not a rule and not a price: nothing in this table changes a
-- total, and a screen that ignores it still sells correctly. It lives in the
-- snapshot so the suggestion ships with the menu that was activated, instead
-- of being invented by whichever client happens to be rendering, and so every
-- channel offers the same thing.
--
-- Deliberately universal. A burger recommending fries and a printer
-- recommending toner are the same row; nothing here knows what a restaurant
-- is (ADR 046).
CREATE TABLE catalog_product_recommendations (
    snapshot_id             TEXT    NOT NULL REFERENCES catalog_snapshots (snapshot_id)
                                    ON DELETE CASCADE,
    -- The product being shown when the suggestion is offered.
    product_ref             TEXT    NOT NULL,
    -- What to offer. Resolved at commit like every other reference: an
    -- unresolvable ref refuses the snapshot rather than activating a menu
    -- that suggests something nobody can sell.
    recommended_product_ref TEXT    NOT NULL,
    -- Declared order. What a screen offers first is a merchandising decision,
    -- so it is content and it is hashed.
    display_order           INTEGER NOT NULL,
    PRIMARY KEY (snapshot_id, product_ref, recommended_product_ref),
    FOREIGN KEY (snapshot_id, product_ref)
        REFERENCES catalog_product_presentation (snapshot_id, product_ref)
        ON DELETE CASCADE,
    FOREIGN KEY (snapshot_id, recommended_product_ref)
        REFERENCES catalog_products (snapshot_id, product_ref) ON DELETE CASCADE,
    -- A product recommending itself is a merchandising mistake, not a loop to
    -- be tolerated at render time.
    CHECK (product_ref <> recommended_product_ref)
);

CREATE INDEX ix_catalog_product_recommendations_product
    ON catalog_product_recommendations (snapshot_id, product_ref, display_order);
