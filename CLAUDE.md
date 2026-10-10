# PI.Edge — engineering rules

Permanent instructions for this repository. Read before changing anything.

PI.Edge is the local POS runtime for PI.COMMERCE: a headless Rust core that
owns the money, with thin clients around it.

## Architecture

```
React (tauri-pos) ─┐
edge-cli ──────────┼─> EdgeClient ─> IPC ─> edge-daemon ─> application ─> domain
                   │                                           │
                   └── never reaches further than EdgeClient    └─> edge-storage (SQLite)
```

`edge-daemon` is the only process that opens the database. A UI or the CLI is a
client of the Edge Protocol and nothing more.

## Boundaries that do not move

* **`edge-domain` is pure.** No SQLite, HTTP, Tauri, Android, filesystem or
  clock, and it never sees JSON, sockets or pipes. Time arrives as a parameter.
* **The core does not depend on Tauri or Android.** `apps/tauri-pos` is
  excluded from the Cargo workspace so this cannot regress by accident.
* **A UI never** opens SQLite, computes an official total, decides payment
  state, or issues a command while disconnected. It renders what the core
  returned, verbatim.
* **Responsibilities never shift between daemon and UI.** If a client needs a
  number, the core computes it and sends it.
* **Money is `i64` minor units.** No floating point anywhere in the core. One
  rounding policy: `edge_domain::money::div_round_half_away_from_zero`.
* **Server-owned ids** (tenant, store, register, operator, product, price book,
  promotion) are opaque strings PI.Edge never mints. Public ids are UUIDs.
* **No cardholder data.** No PAN, CVV, track data or terminal payload is
  stored, logged, journalled or put in a DTO.
* **Hardware sits behind ports injected at the composition root**, and each
  port arrives *with* its adapter. A device failure never changes a sale, a cent
  or a unit of stock, and only `edge-hardware::escpos` may contain an escape
  sequence.

See `docs/FROZEN-DECISIONS.md` for the invariants settled per milestone, and
`docs/ADR/` for why each one was chosen. Do not rewrite an old ADR to make a
later decision look inevitable; amend it.

## Organisation

Group code by **stable responsibility and business domain**, not by task.

```
crates/edge-payments/   payment.rs attempt.rs settlement.rs provider.rs …
crates/edge-cash/       movement.rs position.rs safe_drop.rs close.rs …
crates/edge-inventory/  movement.rs balance.rs error.rs …
crates/edge-hardware/   device.rs scanner.rs printer.rs drawer.rs escpos.rs …
```

Do **not** create `foo_command.rs`, `foo_query.rs`, `foo_mapper.rs`,
`foo_validator.rs`, `foo_helper.rs` or `foo_utils.rs` just because those are
distinct concepts. A cohesive file of a few hundred lines beats ten coupled
files of forty.

The test for splitting anything is:

> Does this piece have a responsibility independent enough that understanding,
> changing or testing it separately actually reduces coupling?

If no, it stays with the context it belongs to.

* **One command per file: no.** Related use cases of one context live in one
  service file (`cash_service.rs`, `payment_service.rs`).
* **Helpers stay local.** A helper with one consumer is private to its module,
  not a public utility.
* **Abstractions answer existing boundaries.** No trait, crate, module, DTO or
  wrapper "in case it is useful later". A trait with no implementation does not
  constrain anything; it only looks like it does.
* **Crates** exist for a real domain, dependency or runtime boundary.
  `edge-payments`, `edge-cash`, `edge-inventory`, `edge-refunds` and
  `edge-hardware` qualify. A feature does not.
* **Visibility** is as small as it goes: private, then `pub(crate)`, then `pub`
  only with a real external consumer.
* **Imports** are clean and grouped. No re-export that exists only to shorten a
  path, and no module that only re-exports another.
* **Tauri** is organised by cohesive functional modules — not one file per
  command, and not a formal plugin per application module.

## Deliberately not built

No legacy kept "just in case": when a path is replaced, the old one is deleted
in the same milestone. Minimal own development — do not build what is not
asked for, and do not stub what has no use case yet.

Currently out of scope: an **HTTP sync transport** — the sync engine and its
transport contract exist (milestone 11), the HTTP client, URLs, routes,
authentication and tenant routing do not, and there is no fake server; retention
and purge of synced facts; ERP, coupons, loyalty, customer pricing; store-wide
inventory coordination (Store Hub, shared ledger, reservations); the receipt
lifecycle, CFDI printing; real Mercado Pago, payment terminal hardware, customer
display, KDS, scales, Bluetooth/NFC, generic serial frameworks, network printer
auto-discovery, remote hardware management; RBAC, EOD scheduler, production
checkout UI.

Never: Flutter, Dart, Odoo, ERP logic in the core, local microservices,
overengineering.

## Working agreement

* Each milestone ends with a report and waits for review. **Never commit
  without explicit authorisation.**
* No global installs without warning first.
* If an architectural decision is not covered by the brief, stop and present
  2–3 options with a recommendation before implementing.
* State what was left out and why. Report test failures with their output.

## Gates — all must be green

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace

cd apps/tauri-pos && pnpm lint && pnpm typecheck && pnpm build
cd apps/tauri-pos/src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

Clippy runs with warnings as errors; `unwrap`/`expect`/`panic!` and float
arithmetic are denied outside tests. Golden fixtures are regenerated only
through `UPDATE_GOLDEN=1`, and a diff that touches a money field is a bug until
proven otherwise.
