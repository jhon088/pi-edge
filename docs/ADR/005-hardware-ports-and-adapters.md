# ADR 005 — Hardware lives behind ports and adapters

Status: accepted · 2026-10-08
Amended in milestone 7: the decision stands, the crate does not.
Amended in milestone 10: the ports arrived, with adapters. See the two
*Amendment* sections at the end.

## Context

Printers jam, drawers stay shut, customer displays disconnect, payment
terminals answer "I don't know". In the legacy app device calls were made inline
from UI code, so a paper-out condition could abort a sale that was already paid,
and the iMin SDK leaked into business logic.

## Decision

`edge-hardware-api` declares traits only — `PrinterPort`, `ScannerPort`,
`CashDrawerPort`, `CustomerDisplayPort`, `PaymentTerminalPort` — plus a typed
`HardwareError`. No concrete adapter is implemented in this milestone, by
design.

Rules:

* The core talks to traits; vendor SDKs (iMin, Android host, Windows, OPOS) live
  in adapter crates outside the core and are injected at the composition root.
* Hardware failure is never a domain failure. A sale is `FINALIZED` on the
  strength of its payments and its committed database transaction; receipt
  delivery is a separate step (`RECEIPT_COMPLETED`) that can fail and be
  retried.
* The printer receives an already-rendered `ReceiptDocument`. The core renders;
  the adapter prints.
* The scanner is modelled as a pull-based queue (`try_next_scan`), so no device
  callback ever runs inside a domain transaction.
* `PaymentTerminalPort` has an explicit `Unknown` outcome and a `lookup` by
  idempotency key. An unknown result is never treated as approved; it must be
  reconciled.

## Consequences

* The whole POS can be tested, and sold, with no hardware attached.
* Supporting a new device is a new adapter crate; the core does not change.
* Asynchronous device behaviour needs an explicit design (queues, timeouts,
  reconciliation) rather than being hidden behind a blocking call. That is extra
  work at integration time and the reason refunds/terminal payments are not in
  this milestone.

## Amendment (milestone 7)

The rule above is unchanged: hardware will sit behind ports injected at the
composition root, and no device call will ever be made from UI code or from
inside a use case.

What changed is when the traits get written. `edge-hardware-api` shipped as a
crate of port traits with **no implementation and no consumer** — nothing
depended on it, nothing implemented it, nothing injected it. Seven milestones
later that was still true, so it was removed.

A trait with no implementation does not constrain anything; it only looks like
it does. The real shape of `PrinterPort` will be decided by the first printer
we actually talk to, and writing it beforehand guarantees it is wrong in ways
nobody can see yet. The boundary is recorded here and in `CLAUDE.md`, and the
port arrives with the adapter.

## Amendment (milestone 10)

The ports arrived, with adapters, in `crates/edge-hardware`:
`PrinterPort` and `CashDrawerPort`, an ESC/POS renderer and TCP transport, an
ESC/POS drawer kick, the structural iMin bridge, and simulators (ADR 035).

Three details of the original decision were refined by the first real device:

* **"The printer receives an already-rendered `ReceiptDocument`"** is now: the
  application expresses a canonical `PrintDocument`, and the **adapter** renders
  the physical protocol. Rendering ESC/POS in the core would have put a vendor
  protocol in the core, which is what this ADR was avoiding (ADR 039).
* **"The scanner is modelled as a pull-based queue (`try_next_scan`)"** did not
  survive contact with the device. A keyboard-wedge scanner has no handle to
  poll: it types into the focused window. There is no `ScannerPort` at all —
  the host captures, a deterministic parser decides, and the result travels as
  an event (ADR 036).
* **`CustomerDisplayPort` and `PaymentTerminalPort` are still absent.** Both
  were in the deleted crate, neither has an adapter, and writing them again
  would repeat exactly the mistake milestone 7 corrected.

What is unchanged, and was the point: hardware failure is never a domain
failure. A sale is `FINALIZED` on the strength of its payments and its committed
transaction, and paper and solenoids come afterwards (ADR 037).
