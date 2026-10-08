# ADR 014 — Price books and the pricing runtime

Status: accepted · 2026-10-08

## Context

Milestone 3 put a price on the product row. That was enough to prove the
catalog pipeline, and wrong as a final model: a product's identity does not
change when a store runs a promotion or when a price increase takes effect next
Monday. Carrying `unit_price` on `catalog_products` forced one price per
product per snapshot, so a store-specific price or a scheduled change meant a
whole new snapshot.

## Decision

**Pricing is a separate concern with its own objects.**

```text
PriceBook   price_book_id, code, name, currency, scope, priority, active
PriceEntry  price_book_id, product_ref, amount, price_mode, valid_from?, valid_until?
```

* A product no longer owns a price. `catalog_products.unit_price_minor` and
  `currency` are gone (migration 0003), not deprecated.
* **`price_mode` moved from the tax profile to the price entry.** Whether a
  quoted amount includes tax is a property of the amount, not of the rate: the
  same 16% profile now serves a tax-inclusive shelf price and a tax-exclusive
  wholesale price. Tax profiles state a rate and nothing else.
* **Scope is two levels**: `TENANT` and `STORE`. No arbitrary hierarchy until a
  requirement asks for one.
* **Resolution is deterministic**, and it is the whole rule in one function
  (`edge_catalog::pricing::resolve_price`):
  1. candidates are entries of *active* books whose scope covers the store,
     whose currency matches, and whose validity window contains
     `effective_at`;
  2. none → `PRICE_NOT_FOUND`;
  3. rank by **scope first** (a store book always beats a tenant book), then by
     **priority** (higher wins);
  4. a tie at the top → `AMBIGUOUS_PRICE`.

  Scope outranking priority is the deliberate choice: "the store price
  overrides the base price" becomes structural rather than something that holds
  only while someone maintains the priority numbers. Priority breaks ties
  *within* a scope.
* **Validity is `[valid_from, valid_until)`** — lower bound inclusive, upper
  bound exclusive. A replacement price can therefore start exactly when the
  previous one ends, with neither a gap nor an overlap. Two windows that really
  do overlap are a data error and surface as `AMBIGUOUS_PRICE` rather than a
  coin flip.
* **`effective_at` is an explicit parameter.** The domain never reads a clock.
  Tests place themselves anywhere in time, and a sale can pin the instant it
  prices at.
* **A sale captures `effective_at` as well as the snapshot.** ADR 013 pinned
  *which* catalog prices a sale; pricing with validity windows means we must
  also pin *when*, or a price scheduled for midnight would move the price
  between two scans of the same sale. `PricingCapture { snapshot_id,
  catalog_version, effective_at }` is written with the transaction and survives
  a restart.
* **The import carries pricing.** `BeginCatalogImport` declares the price books
  (few, like tax profiles); each product record carries its own prices, so a
  product and its prices always land in the same chunk and the same database
  transaction. The canonical content hash gained a price-book section and a
  price-entry section, so two snapshots that differ only in a price are
  different snapshots.
* **A snapshot must declare at least one price book, and a commit verifies the
  price counts.** A catalog that can sell nothing is refused at the door rather
  than at the till.

## Consequences

* Snapshots imported before migration 0003 keep their products and barcodes but
  have no price books, so every scan against them answers `PRICE_NOT_FOUND`
  until the catalog is re-imported. The migration does not invent a price book
  from the old column: PI.Edge does not author master data, not even during a
  migration. The failure is loud and the fix is one `import-catalog`.
* Two columns had to be dropped and both carried `CHECK` constraints, which
  SQLite will not remove with `ALTER TABLE ... DROP COLUMN`. Migration 0003
  rebuilds `catalog_products` and `catalog_tax_profiles` and uses
  `PRAGMA defer_foreign_keys = ON`, because `PRAGMA foreign_keys = OFF` is a
  no-op inside a transaction and migrations run in one.
* `PriceResolver`/`BasePriceResolver` are gone. The seam they stood for is now
  `resolve_price` returning `ResolvedPrice`; promotions will compose over that
  value, and the transaction aggregate still only ever receives a unit price, a
  rate and a mode.
* `price_entries` needs a surrogate key: its natural key includes a nullable
  `valid_from`, and SQLite does not enforce uniqueness over NULLs in a primary
  key. A unique index on `IFNULL(valid_from, '')` closes that.
* Still deliberately absent: promotions, coupons, loyalty, customer-specific
  pricing, quantity breaks. The context object already carries `quantity`, so
  quantity breaks are the next natural implementation of the same rule.
