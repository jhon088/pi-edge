# ADR 044 — The catalog says what is offered, in which language, with which picture, and what it asks

Status: accepted (milestone 13A)
Extends ADR 011 and ADR 012. Does not change ADR 013 or ADR 015.

## Context

One core must serve a retail counter, a retail kiosk, a restaurant counter, a
restaurant kiosk, a venue counter and a venue kiosk. The decision that there is
no second runtime and no second transaction engine means every one of those
needs the *same* pricing, promotions, payments, inventory, receipts, refunds,
recovery and sync — and therefore that whatever a restaurant kiosk needs beyond
retail has to be expressed in the catalog, not in a parallel system and not in
a vertical-specific module.

Four things were missing, and the audits found all four:

1. **Choices.** A burger is sold with a size, extras, a side and a drink, and
   choosing the drink can ask for its size. Retail has no model for a product
   that asks questions before it can be sold.
2. **Channel.** The same catalog is browsed by a counter and by a kiosk, and
   they do not offer the same things. Nothing expressed "we do not sell that
   here", and the obvious wrong answer — reusing inventory — would make a
   de-listed item look like a stock problem and a sold-out item vanish from the
   catalog.
3. **Presentation.** A kiosk is operated by a customer, so it needs categories,
   a photo and the customer's language. Retail needed a product name and that
   is all it has.
4. **Localization.** Freezing `name: String` now would mean rewriting every
   content row, every DTO and the content hash later.

## Decision — the names are universal, the verticals are data

`SelectionGroup`, `SelectionOption`, `Channel`, `ChannelAvailability`,
`Category`, `LocalizedText`, `AssetRef`. There is no `RestaurantModifier` and
no `RestaurantMenu`, because there is nothing a restaurant needs here that a
venue selling a ticket with a seat tier, or a retailer selling a cut-to-length
cable, does not express with the same four tables.

A `ChannelRef` is an opaque upstream string, like a store or a price book. The
core never branches on `"kiosk"` or `"pos"`: it asks the catalog what a channel
offers and does what it is told. That is the only way one core serves three
verticals without knowing which one it is running.

## Decision — a question is a quantity range, not a rules engine

```text
SelectionGroup   min_quantity .. max_quantity, in UNITS, across the group
SelectionOption  max_quantity, default_quantity, optional component_ref
```

Every scenario the kiosk brief listed is a pair of integers:

| Scenario | Group | Option |
| --- | --- | --- |
| exactly one size | `min 1, max 1` | — |
| up to five extras | `min 0, max 5` | — |
| two of the same extra | — | `max_quantity 2` |
| an ingredient removed | `min 0, max 1` | `default_quantity 1`, no component |
| a combo | one group per part, each `min 1, max 1` | — |

`min_quantity >= 1` **is** what required means. There is no `required` flag,
because a flag and a minimum can contradict each other and one of them would
then have to win.

Nesting is a group whose owner is an *option* rather than a product. That is the
entire mechanism behind `Bebida -> Coca Cola -> Tamaño`: no tree type, no
condition language, no rule evaluator. A cycle in that chain is refused at
commit, which is the only place it is detectable.

## Decision — a chargeable option names a product; it never carries a price

An option that costs money sets `component_ref` to a product the snapshot
already contains, and the **existing** price books price it. There is no price
column on an option, no delta, no second price-book engine, and nothing on the
wire by which a client could state what an extra costs. An option with no
component is purely semantic and free: *no onions* is not a product.

This is deliberate and it is the whole reason the kiosk needs no new money
code. Promotions, tax profiles, price modes, scope and priority already apply
to a component exactly as they apply to a scanned barcode.

What this milestone does **not** do is compute a configured line price. The
line still prices `unit_price × quantity`. Carrying a base price, the selected
options and a configured unit price on `TransactionLine` — so a promotion can
distinguish BASE_ONLY from CONFIGURED_TOTAL — is a separate milestone, and
guessing at it now would freeze the wrong shape.

## Decision — commercial availability is not inventory

```text
catalog_channel_availability   "we do not sell that here"   catalog content
inventory_movements            "there is none left"         this installation's ledger
```

Two questions that look alike and must never share a model. A product with
stock can be off the kiosk's menu; a product that ran out today is still on
tomorrow's menu (ADR 029 owns the second one).

A channel declares its own default as data — `OFFERED` or `WITHHELD` — so
absence is never ambiguous. A counter offers everything except what is
withdrawn; a new self-checkout lane may offer nothing until somebody curates
it, and "sell everything by default" would be the wrong answer on its first
day. A snapshot with no channels filters nothing, which is every retail
catalog.

`SNOOZED { until }` is the one state a store operates by hand — sold out until
— and it carries its own deadline so nothing has to guess when it lifts. A
product is offered only if the product *and* its category are offered, and an
unknown channel offers nothing rather than everything.

## Decision — localization is resolved now, and it is content

`LocalizedText { default, translations: BTreeMap<Locale, String> }`. Resolution
is an exact locale match or the default — **no prefix matching**, because `es`
is not `es-MX` and guessing is worse than falling back. Translations live in
rows (`catalog_text_translations`), not a JSON blob, so the snapshot stays
queryable and is never held in memory.

Every translation participates in the content hash, fed in locale order, so the
order of a JSON object on the wire cannot change a snapshot's identity.

## Decision — an image is a reference, never bytes

`AssetRef { content_hash, media_type, byte_size }`, validated as
`sha256:<64 hex>`. SQLite is not an asset store. The reference participates in
the hash, so a changed photo is a changed snapshot and a client may cache by
hash. Fetching the bytes is an Asset Runtime and is not this milestone.

## Decision — one importer, extended

`begin -> put_chunk -> commit (counts + hash) -> activate`, unchanged. Channels
and categories travel in the header, with the tax profiles and price books:
there are few of them and the chunks reference them. Presentation, selection
groups, selection options and availability are bulk content and travel in the
chunks.

Validation is split by what can actually be decided where:

| Where | What | Why there |
| --- | --- | --- |
| the value object | a group that accepts nothing, an inverted range, a default above its own maximum | an impossible record cannot even be built to be sent |
| SQLite | presentation without its product, an option without its group, a chargeable option without its product, availability on an undeclared channel, a snooze with no end | a foreign key or a CHECK sees it, so no adapter bug can persist it |
| commit | a group hanging from an option that never arrived, a cycle, an availability target that does not exist | only a complete snapshot can answer it |

Commit-time validation runs **before** the snapshot is marked `PENDING`, so a
structurally invalid import stays `IMPORTING` and resumable, and never prices a
sale. It costs nothing for a retail catalog: with no extension rows stored
there is nothing to load.

## Decision — a retail snapshot keeps the hash it already had

The six new hash sections contribute **nothing at all** when they carry no
rows. A catalog that uses none of the extensions therefore hashes to exactly
what it hashed before they existed.

This is not a convenience for the tests. `catalog_snapshots.content_hash` is
persisted and `begin_catalog_import` is idempotent on
`(store, version, declared_content_hash)`. If the hash of unchanged content
moved, an upgraded producer re-declaring version *N* would compute a hash that
disagrees with the row the store already activated, and the import would
collide on `UNIQUE (store_id, version)` instead of being recognised as a
replay. Each section announces itself with its own tag, so two semantically
equal snapshots still hash identically either way.

## Consequences

* `SnapshotCounts` and the wire `SnapshotCountsV1` gain six fields, all
  defaulted, so a producer that has never heard of any of this still commits.
  The only golden-fixture change in this milestone is those six zeros; no money
  field and no hash moved.
* Migration 0012 is purely additive: seven new tables and ten defaulted
  columns. Nothing is rebuilt, so no existing snapshot, price, sale or ledger
  row is touched.
* The core still has no concept of a *configured* sale line. A kiosk cannot yet
  sell a burger with bacon — it can only be told which questions the burger
  asks and what the answers cost. That is the next milestone, on purpose.
* Nothing here downloads an image, renders a menu, routes to a kitchen, or
  assigns an order number.
