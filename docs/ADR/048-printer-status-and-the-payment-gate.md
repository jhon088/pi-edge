# ADR 048 — The printer answers for itself, and a showroom kiosk will not take money it cannot print a ticket for

Status: accepted (showroom — PX Kiosk)
Extends ADR 035, ADR 037, ADR 039 and ADR 043. Changes no decision in any of
them.

## Context

A kiosk on a stand took a customer's money, finalized the sale, and then found
it had no paper. ADR 043 says that is *correct*: the receipt is a separate
obligation, the sale stands on its payments and its committed transaction, and
paper failing afterwards must not touch a cent. The core did exactly what it
promised.

It is still the wrong thing to have happened to the person standing there. A
self-service lane with nobody behind it has no way to hand over a receipt by
other means and nobody to explain. So the brief was explicit: **do not let a
customer pay for a ticket that cannot come out.**

Two things were in the way.

**Nothing read a sensor.** `/demo/status` published `"paper": "UNKNOWN"` and
`"jammed": null` as literals. `PrinterModel::reports` declared what a model
*could* say and nothing asked it. The only signal the runtime had was "the
spooler accepted the job", which is not a sensor — treating it as one is the
specific mistake ADR 035 exists to forbid.

**The wire is write-only.** The receipt printers on the stand are USB 3nStar
units, and every desktop OS exposes a USB receipt printer as a print queue.
`EscPosSpoolerPrinter` hands bytes to that queue, and a queue cannot carry a
`DLE EOT` reply back out. So "just read the status" was not available.

## Decision

### 1. Three status sources, named, and none pretends to be another

`PrinterStatusPort` is a second capability beside `PrinterPort`, because asking
and printing are not the same act and on this hardware are not the same wire.

```text
PrinterStatusPort ─┬─> UsbDleEotStatus ─> USBPRINT  ─> DLE EOT ─> the firmware
                   ├─> EscPosTcpStatus ─> TCP 9100  ─> DLE EOT ─> the firmware
                   └─> SpoolerStatus   ─> PRINTER_INFO_2W.Status
```

Two of them are real **bidirectional** channels and send `DLE EOT 1..4`,
folding the four replies into one status: online, the cover, the roll's
near-end and end sensors, the cutter, a recoverable error, an unrecoverable
one. They are the device's own answer, and the only sources that know paper is
*nearly* out.

* **`UsbDleEotStatus`** is the one that matters for the hardware the showroom
  has. A USB receipt printer registers a **USBPRINT** device interface, which
  is a file: open it, write the three bytes, read the reply. The FFI
  (`SetupDiGetClassDevsW` to enumerate, `CreateFileW`/`WriteFile`/`ReadFile`
  to talk) lives in `edge-winspool`, the only crate permitted `unsafe`.
  Printing is untouched and stays on the spooler — this only asks.
* **`EscPosTcpStatus`** asks a network printer on the socket it prints on.
* **`SpoolerStatus`** reads the host spooler's flags — `PAPER_OUT`,
  `PAPER_PROBLEM`, `PAPER_JAM`, `DOOR_OPEN`, `OFFLINE`, `PAUSED`, `ERROR`,
  `USER_INTERVENTION`.

**The spooler on its own is not `DLE EOT` and is never labelled as if it
were.** It is the fallback for hardware that offers nothing better, it is
`SPOOLER` on the wire, and it is incapable of producing `NEAR_END` rather than
guessing one. A channel configured as `usb` or as an address that cannot be
read reports `NO_RESPONSE` — it does **not** quietly degrade to queue flags,
because a source that claimed the firmware's authority and served a driver's
opinion is the exact confusion this ADR exists to prevent.

An operator picks the channel on the kiosk's own screen, and the host offers
the USB devices it can actually read rather than asking anybody to guess
whether their printer has a bidirectional interface.

### 1a. A `ReadFile` with no timeout is the one hazard, and it is bounded

`ReadFile` on a USBPRINT handle has no timeout to set: it returns when the
driver has something or when it gives up. A firmware that answers nothing could
leave it there — and the mutex below is held across the read, so a hang would
take the *print* path with it.

So the exchange runs on a worker thread with a 1200 ms deadline, and a read
that passes it is **abandoned**: the status reports `NO_RESPONSE`, the mutex is
released, and an `in_flight` flag stops another read being started on top of
the first. The abandoned thread holds the only handle, so the next poll is
refused until it finishes rather than piling threads up one per poll. `DLE EOT`
is a real-time command answered in microseconds; the deadline exists for the
device that is not working.

`status_source` says which source *can* answer, from the channel configured.
`status_checked_at` says whether it has. The two are separate fields because
"this printer cannot say" and "it has not said yet" are different facts and a
floor that cannot tell them apart will read silence as health.

### 2. The fixed bits are load-bearing

Every `DLE EOT` reply has bit 1 and bit 4 set and bit 0 and bit 7 clear.
`is_status_byte` checks that mask and the reader takes the first byte that
passes it. Without that check, line noise or a stale buffer byte becomes "the
cover is open" — which, under the rule below, closes the lane. The validation
is not defensive tidiness; it is what makes the rule safe to act on.

### 3. One mutex, in `SwappablePrinter`

A `DLE EOT` query and a receipt travel down the same wire. A status byte asked
for halfway through a ticket is a status byte printed into a ticket.

So `SwappablePrinter` — already the single indirection every print passes
through — holds one `Mutex<()>` and takes it for a **whole document** (every
copy) and for a **whole status read**. A poll during a busy lunch waits its
turn. This is the only correct home for the lock: it is the one place both
paths cross.

### 4. Asked on three occasions, and only three

Every fifteen seconds while idle; immediately before a payment may start; once
after a document has gone through. The third is the one that catches a roll
running out *on* a receipt — the paper was there when the payment began and is
not there now, and the next customer has to be stopped rather than discover it.

The answer is cached, because the question costs four socket round trips and
`/demo/status` has a three second budget. A payment asks fresh.

### 5. Three conditions stop a payment. A low roll does not. Silence does not.

```text
cover_open  OR  paper == END  OR  state in (NO_RESPONSE, OFFLINE)
```

`PrinterStatus::blocks_payment` is the one place this is written, the core
publishes its answer as `blocks_payment`, and no client re-derives it from the
flags — which is how two clients come to disagree about whether a lane is open.

Two deliberate exclusions:

* **`NEAR_END` sells.** Refusing a customer over a warning would close a lane
  for an hour of perfectly good paper. It is logged and shown in Admin.
* **`UNKNOWN` sells.** An installation with no status channel and a spooler
  that reports nothing knows nothing about its printer. A rule that refuses
  every sale wherever the question cannot be put is a rule that closes the
  shop. **What blocks is an answer, never the absence of one.**

A jam is reported and does not block: it is cleared by hand in seconds, and
both sources agree on that so they cannot disagree about a lane.

### 6. The gate is in the host, not in the core

`require_a_printer_that_can_follow_through` lives in the Tauri host, at
`kiosk_pay` and `cashgenic_pay` — the only two doors from any of the four lanes
to a payment.

Not in the core, and this is the point of the ADR. "A device failure never
changes a sale, a cent or a unit of stock" is a boundary that does not move,
and the core keeps it: a receipt is still an obligation that comes after the
money and can still fail without touching it (ADR 043). What this adds is a
**showroom policy one level out** — do not put a payment screen in front of
this screen's customer when the ticket cannot come out — and the right place
for a policy about this screen's customers is this screen's host.

Not in React either. The screen asks first, as a courtesy, so a customer is
told rather than watching a tender choice fail. But the host refuses the
payment command itself, so a reloaded webview cannot get past it. A rule a
reload can remove is not a rule.

## Consequences

* A kiosk whose printer cannot answer behaves exactly as it did before. Nothing
  about this closes a lane that was selling.
* `/demo/status` publishes eleven printer fields and **none of them is null** —
  `UNKNOWN` and `""` say "nothing answered" more clearly than a missing key,
  and a poller branches without checking for absence.
* The status channel is an operator's setting on the kiosk's own screen, stored
  beside the queue, validated on the way in. A malformed address is refused
  rather than read as `NO_RESPONSE`, which would stop every sale on the
  machine.
* **Serial is not implemented, and is not needed.** The showroom's printers are
  USB, which `UsbDleEotStatus` now reads directly, and network printers are
  covered by TCP. A serial path would be a third transport for hardware nobody
  on this project has, and adding it later is a transport behind the same port
  — not a change to anything decided here.
* **The Windows FFI is compile-checked against `x86_64-pc-windows-msvc` and
  has not been run on a printer.** Said plainly because it is the one part of
  this that a test suite on a developer's Mac cannot exercise: the enumeration
  and the exchange are type-checked for the target, their failure paths are
  covered, and the first real `DLE EOT` byte will be read on the stand.
