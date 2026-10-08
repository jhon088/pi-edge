# ADR 011 — External product master, canonical local projection

Status: accepted · 2026-10-08

## Context

Product data in this business originates in ERP and master-data systems, is
consolidated by PI.COMMERCE Server, and only then reaches a register:

```text
ERP / master systems -> PI.COMMERCE Server -> versioned snapshot -> PI.Edge
```

The legacy POS blurred this: it could create and edit products locally, which
produced registers that disagreed with each other and with the ERP, and made
"which price was this sold at?" unanswerable.

## Decision

**PI.Edge does not create product master data.** Not a product, not a barcode,
not a price, not a tax rate. The local catalog is a *projection* of content
produced upstream, and the local API offers no way to author it — the only
inbound path is importing a snapshot (ADR 012).

* **Identity is `product_ref`**, an opaque reference assigned upstream. PI.Edge
  never mints one. It is the same type the sale lines already carried, so the
  transaction schema did not change.
* **`external_ref` is metadata.** It carries the source system's own code for
  traceability and reconciliation. It is never operational identity, never a
  lookup key on the sale path, and the core attaches no meaning to it.
* **Barcodes are many-to-one.** A product may carry several codes; a code
  belongs to exactly one product within a snapshot, enforced by the primary key
  `(snapshot_id, barcode)`.
* **`active` and `sellable` are distinct.** An inactive product stays in the
  snapshot so historical lookups still resolve; a product that is active but
  not sellable (a service, a component) is refused at the sale with its own
  error code. Deleting rows was rejected: a sale that captured an older
  snapshot must still be priceable.
* **Tax is declared, never inferred.** Each product names a tax profile that
  states the rate *and* whether the quoted price already includes it. Nothing
  guesses a rate from an amount or a product kind.
* **The price is resolved by the core**, from the snapshot, through
  `PriceResolver`. Today the only implementation returns the snapshot's base
  store price. Promotions, price lists and customer pricing will be further
  implementations, and the transaction aggregate will not change when they
  arrive.
* The client sends **identity and quantity**. `AddItem` has no price, no tax
  rate and no description, so a UI cannot be wrong about any of them.

## Consequences

* A register cannot sell a product the server has not sent. That is the
  intended failure: `PRODUCT_NOT_FOUND` with a clear message, not an improvised
  local product.
* Fixing a wrong price is a server-side operation followed by a new snapshot.
  There is deliberately no local override, and adding one later would be a
  decision to record here.
* `edge-catalog` is a separate bounded context (pure, no IO) rather than more
  modules inside `edge-domain`: the sale and the catalogue have different
  lifecycles, different owners upstream, and different invariants.
* Line discounts remain in the domain but are unreachable from the wire API
  until promotions exist, so no client can discount anything today.
