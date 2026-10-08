# ADR 005 — Hardware lives behind ports and adapters

Status: accepted · 2026-10-08

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
