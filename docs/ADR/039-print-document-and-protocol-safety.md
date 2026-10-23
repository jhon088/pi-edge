# ADR 039 — What gets printed, and what a client may ask for

Status: accepted (milestone 10)
Amends ADR 005.

## Context

ADR 005 said "the core renders; the adapter prints", with the printer receiving
an already-rendered `ReceiptDocument`. Rendering **ESC/POS bytes** in the core
would put a vendor protocol in the core, which is the opposite of what that ADR
wanted.

There is also a security question that only appears once a printer is real. A
network printer is a raw TCP socket. An operation that accepted a host, a port
or a byte string would let any client print to any machine on the network, or
send an arbitrary escape sequence to the device.

## Decision — the document is canonical; the bytes are the adapter's

```text
application ──PrintDocument──> PrinterPort ──ESC/POS bytes──> printer
```

`PrintDocument` is lines with alignment, emphasis, double size, a width, a cut
and a copy count. The application composes it; `edge-hardware::escpos` is the
only module in the repository that contains an escape sequence.

**Amendment to ADR 005**: the core expresses the canonical document and
intention; the device-specific adapter renders the physical protocol. The
division of labour is unchanged, the line is drawn one step earlier.

No HTML, no browser printing, no template engine. Non-ASCII text folds to
characters the printer's code page can show, because a receipt printer is not
Unicode and emitting bytes it will render as garbage is worse than folding `ñ`
to `n`.

## Decision — the protocol carries intent, never capability

| Operation | Payload |
| --- | --- |
| `GET_HARDWARE_STATUS` | none |
| `PRINT_TEST_PAGE` | none |
| `OPEN_DRAWER_DIAGNOSTIC` | none |
| `PRINT_TRANSACTION` | `transaction_id`, `reprint` |
| `SUBMIT_SCAN` | `event_id`, `barcode`, `device_id?` |

That is the whole hardware surface. There is no field for a host, a port, an
address, a driver, raw bytes, a method name, a copy count or a paper width, and
a contract test enumerates the payload keys and asserts each is absent.

So a client asks `PRINT_TEST_PAGE`, never `SEND_BYTES_TO 192.168.x.x:9100`.
Configuration lives in the composition root — read once, from the environment,
refusing to start on an unrecognised value — and nothing downstream can reach
it. A test asserts the configured host and port never appear in a status
response.

There is no operation per driver, and none that exposes an adapter's internals.

## Decision — a `PrintDocument` is not a receipt

The receipt printed here is enough to prove the transport and the layout: store,
ticket, register, operator, lines with quantity and amount, discounts, totals, a
`** COPIA **` banner on a reprint. No fiscal block, no QR, no CFDI.

The commercial and fiscal lifecycle of a receipt — when it is owed, what it must
contain, what happens if it is never delivered — is its own milestone. M10
deliberately does not reach for it, and in particular does not wire
`RECEIPT_COMPLETED`, which exists in the transaction aggregate and is still
driven by nothing.

> **Amended by ADR 043 (milestone 12).** That milestone arrived: the receipt is
> an obligation of the closed sale, `RECEIPT_COMPLETED` was removed rather than
> wired, and the fiscal half is still out of scope.

## Consequences

* A second printer protocol is a second renderer, and the application changes
  nothing.
* A UI can safely expose "print test page" and "open drawer" buttons, because
  neither can be made to do anything else.
* The receipt's content will change when its own milestone defines it. Nothing
  persisted here depends on its current shape.

> **Extended by ADR 040 (milestone 11).** The same boundary holds against the
> *server*, not only against clients: hardware is not in the inbound sync
> contract. A printer host, a drawer driver, an iMin bridge or a device topology
> is local to the composition root, and no message from PI.COMMERCE Server can
> set one.
