# ADR 047 — A second vertical is a channel and a shell, not a second application

Status: accepted (milestone 13C-lite / Expo MVP — the retail lane)
Extends ADR 044 and ADR 046. Changes no decision in either.

## Context

The Expo MVP had to show a second vertical: a **retail self-checkout**, beside
the restaurant kiosk that already existed. A supermarket lane and a burger
kiosk look nothing alike — one has a scanner and a running total, the other has
photographs and questions — and the easy answer is to build a second
application.

That answer is wrong here for the same reason ADR 044 gave: the money is the
same money. Pricing, promotions, taxes, payments, finalization, receipts,
refunds, inventory, the journal, the outbox, recovery, the lock, the session and
the business day are not vertical-specific, and a second application would
either duplicate them or depend on them badly.

So the question was narrower than it looked: **what actually differs between
the two lanes**, and where does the difference live.

Three things differ, and only three:

1. **What is for sale.** Burgers at a restaurant kiosk, groceries at a lane.
2. **What the customer sees.** A photo-led menu with a configurator, against a
   scan list with a total pinned beside it.
3. **How a sale starts.** A tap on "empezar", against a barcode crossing the
   glass.

Everything else — every call that touches money — is identical.

## Decision — one snapshot, two channels

A store may have **exactly one active catalog snapshot** (`0002_catalog.sql`,
unique index on the active row). That is not an inconvenience to work around;
it is the invariant that makes "what did this sale cost" answerable. So the two
verticals are **not** two catalogs swapped underneath a running till. They are
two channels of one snapshot:

```text
one ACTIVE snapshot
  ├── KIOSK          restaurant: C-BURGERS, C-COMBOS, C-SIDES, C-DRINKS
  ├── KIOSK-RETAIL   retail:     R-BEBIDAS, R-SNACKS, R-DESPENSA, …
  └── COUNTER        both: a counter sells whatever the shop sells
```

Channel availability already existed for exactly this — "we do not sell that
here" (ADR 044) — so the separation needs no new mechanism, no new table and no
new query. Each lane reads the menu for its own channel and the other lane's
aisles are simply not on it.

### Availability resolves product **and** category, so a lane defaults to OFFERED

`AvailabilityIndex::offers_product` is a conjunction: the product must be
offered *and* its category must be. A product with no row of its own falls back
to the channel's default, so a channel that defaults to `WITHHELD` shows
**nothing** however many of its categories are marked `OFFERED`.

A lane therefore defaults to `OFFERED` and withholds the other lane's
categories explicitly — six rows and four rows, one per category, and no rows
at all per product. The first attempt had `KIOSK-RETAIL` defaulting to
`WITHHELD` with its own categories offered, and the retail menu came back empty
with every row in the database looking correct.

## Decision — switching verticals is a host preference, not core state

Which lane an installation is is stored in `vertical.json` in the app's data
directory, read and written by the host, and it names both the vertical and the
channel it sells from. It is **not** in the core: it changes nothing the core
computes, so a settings table, a protocol operation and a migration would all
be ceremony around a presentation choice. It is not in the webview's storage
either — a kiosk that forgets which shop it is after a frontend reinstall is
worse than useless, and whoever is supporting the machine can read a file.

Switching lanes **abandons the open sale**, deliberately, in the host. A basket
half-built at a self-checkout must not reappear on a restaurant menu where none
of it is sold, and leaving it for the next shell to discover would make the
failure arrive later and further from its cause. Nothing else is touched: no
re-import, no re-activation, and the catalog, the photographs, the printer
settings, the till session and the business day all survive it.

## Decision — the lane opens on the first scan, and the host opens it

The core does not open a sale for a scan. It refuses with
`SCAN_NO_ACTIVE_TRANSACTION`, because no product decision says a scan should
create a sale (ADR 036) — and that stays true.

But a self-checkout has no "start" button: the first barcode *is* the start. So
the host orchestrates the two operations that already exist — ask whether a sale
is open, open one if it is not, then submit the scan — and adds no transaction
logic of its own. If the scan is then refused and *this call* was the one that
opened the sale, the host abandons it again, so a barcode nobody recognises
never leaves an empty basket sitting on the lane.

The search screen's "añadir" does the same thing for the same reason: looking
something up is as good a start as scanning it.

## Decision — one payment journey, shared by both shells

There is no retail payment, no retail totalizer and no retail finalize. The
journey — the wait for the tap, the authorization, the one `pay` command, the
receipt that is a separate act after the money — lives in `usePaymentJourney`
and both shells call it. It was lifted out of the restaurant shell unchanged
rather than copied, because a second copy of the one path that moves money is
the most expensive kind of duplication there is.

The confirmation screen is also shared, and knows which lane it is only well
enough to say "Gracias por tu compra · Ticket" instead of "Tu pedido está en
preparación · Pedido".

## Decision — the demo dataset is additive

`demo/retail/menu.json` carries only the retail lane's own rows — channels,
categories, products, presentation, availability — and `demo-seed` appends its
list sections to the restaurant dataset before importing one snapshot. A
missing `demo/retail/` is not an error; the restaurant simply seeds alone.

Barcodes are generated, not transcribed: `750` + index + an EAN-13 check digit,
so every code in the dataset is one a real scanner will accept and can be typed
in by hand at a stand with no reader attached. A wrong check digit is the
failure that hides — the reader never emits the code, and nothing reaches a log
— so a test computes all of them.

## Decision — a shipped dataset reconciles its version; a real one asserts it

A catalog version is an identity. `begin_catalog_import` returns the existing
snapshot when the version *and* the content hash match — which is what makes an
import resumable and a re-run free — and refuses with
`CATALOG_VERSION_CONFLICT` when a version it already holds arrives with
different content. That refusal is correct and stays: a version whose content
can change is a version that cannot answer "what did this sale cost".

But it made the dataset's `"version": 1` a problem the moment the retail
vertical changed what version 1 *is*. Every installation seeded before this
release holds version 1 with the restaurant-only content, so the seed asked for
a number it could not have.

So who picks the number depends on who owns the content:

* **A real catalog** keeps `VersionPolicy::Declared`. Versions are assigned
  upstream, and a collision is a genuine error that must surface.
* **The demo dataset** ships *with* the application and changes between
  releases, so the number in the file is a default rather than an assertion. It
  uses `VersionPolicy::Reconcile`: reuse the version already holding this exact
  content, otherwise take the version in the file if it is free, otherwise
  `highest + 1`.

Checking for matching content **first** is what makes it idempotent. Asking
"is this version taken?" first would mean every run after an upgrade took
another number, and a kiosk that has been demonstrated fifty times would be on
version fifty-one.

`highest` has to come from the core, because `get_catalog_status` shows the
active, pending and importing snapshots and a **superseded** snapshot appears in
none of them while still owning its version forever. Hence
`CatalogStatusV1.highest_version` — a read any catalog producer needs, not just
this one. The old snapshot is superseded on activation, never rewritten and
never deleted, so the sales that captured it still price against what they
captured.

### The demo will not adopt an installation it does not own

`demo-seed` supersedes the catalog in force, and the catalog in force prices
real sales. Nothing on a snapshot says "a demo made this", and inventing such a
field would be a migration and a protocol change in service of a dev tool. What
already exists is the device profile: the seed provisions exactly one
tenant/store pair, and `init_device` returns an existing profile untouched
rather than overwriting it. So a database whose device is not
`TENANT-NOVA`/`STORE-NOVA-01` is refused before a single asset is imported.

**The limitation, stated rather than hidden:** a real catalog imported onto
*that* tenant and store would be indistinguishable from the demo's own and
would be superseded. Nothing but the demo should ever be `TENANT-NOVA`.

## Decision — the camera preview is a local setting that defaults to off

A self-checkout shows the customer their own camera, because seeing yourself on
a lane is what makes people believe the shop can see them too. So the retail
lane can show a small mirrored preview in the corner.

It is **off unless an operator turns it on**, and that is the whole design.
A kiosk that asks for camera permission because it happens to be installed on a
laptop with a webcam is a kiosk nobody trusts, so `shouldStream(enabled,
vertical)` is consulted before anything is requested and `getUserMedia` is
never called while the answer is no. The same predicate is why the restaurant
never has a preview: it returns false for that lane whatever the setting says.

What the preview is: one `<video>` element, mirrored, fed by one stream.
Nothing is recorded, no frame is read, no still is captured, nothing is stored,
and no byte reaches the core, the daemon, the database or the network. There is
no device id in the constraints — a lane has one camera, and naming a specific
one would be worse than naming none.

A refusal is an answer, not a failure. `NotAllowedError` becomes "permission
denied" for Admin to show; anything else becomes "unavailable"; neither throws,
and the lane keeps selling either way. The customer is never shown a modal
about it — the corner is simply empty.

The setting lives in `preferences.json` beside the database, with the vertical,
because both are the same kind of thing: a choice about how this box behaves
that changes nothing the core computes. Stopping is deliberate and happens on
every way out — switched off, switched to the restaurant, or the window
closing — and a stream that arrives *after* a stop (the permission prompt is
modal and slow) is closed rather than kept.

## Consequences

* A retail lane and a restaurant kiosk are the same installation, the same
  daemon, the same database, the same session and the same till. Switching is a
  file write and a remount.
* `COUNTER` now offers both lanes' goods, which is what a counter in a shop
  that sells both would do.
* The two lanes cannot see each other's goods, and that is enforced by a test
  rather than by convention.
* Seeding one installation imports both verticals' assets: 15 restaurant
  photographs plus 22 retail packshots. An installer that ships only
  `demo/menu.json` would leave the retail channel empty with nothing failing
  until someone switched lanes on a stand, so the bundle's resource list is
  asserted too.
* A third vertical is this shape again: a channel, its own categories, a shell,
  and a line in the switch. It is not a new application, and nothing in the
  core needs to know it exists.
