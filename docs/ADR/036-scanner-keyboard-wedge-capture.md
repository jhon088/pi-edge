# ADR 036 — Capturing a keyboard-wedge scanner

Status: accepted (milestone 10)

## Context

The scanner we have works, physically, in the previous POS. It is a USB HID
device in **keyboard-wedge** mode: it types the barcode's characters into
whatever window has focus and sends a terminator. It is not a serial device and
there is no handle to read.

That is awkward for a headless core. A daemon cannot portably read keystrokes
aimed at another process: macOS requires Input Monitoring consent, Linux needs
udev rules over `/dev/hidraw`, Windows needs Raw Input — and on an iMin terminal
the built-in scanner is a keyboard too.

## Decision — capture in the host, decide in Rust

```text
React (keydown) ─> Tauri host ─> ScanBuffer (edge-hardware) ─> ScanEvent
                                                                  │
                                              SUBMIT_SCAN ─> daemon ─> AddItem
```

The host that owns the window captures, because that is the only portable place
the keystrokes exist. **The rule for what counts as a scan lives in
`edge-hardware`**, in one deterministic parser, used from the host's Rust side.

So React forwards keys and nothing else. It holds no buffer, no timeout, no
terminator list, no minimum length; it does not resolve a product, does not
compute a quantity and does not know what a barcode is. The webview's only
privilege is being where the keys land.

Rejected: forcing a HID daemon, which trades portability and three platform
permission models for the appearance of purity. Rejected: letting React decide a
scan completed, which would put the rule in TypeScript, untested, and leave the
Rust parser with no production consumer. Rejected: one IPC round trip per
character.

The cost is a new dependency: `apps/tauri-pos/src-tauri` now links
`edge-hardware`. That is a pure infrastructure crate, not the application layer;
Tauri still has no path to `edge-application`, `edge-storage` or SQLite.

## Decision — what the parser decides

Every rule is in one place and tested without a device:

* a terminator (`Enter`, `Tab`, `\r`, `\n`) completes a scan;
* **CRLF is one scan**: a terminator on an empty buffer emits nothing;
* a gap longer than the inter-key timeout resets the buffer, so **human typing
  never becomes a scan** — each slow keystroke starts over;
* shorter than the minimum length is not a barcode, and the buffer is cleared;
* longer than the maximum resets rather than growing;
* modifiers and navigation keys are ignored and do **not** interrupt a scan.

## Decision — identity is the event, not the barcode

```rust
ScanEvent { event_id, barcode, occurred_at, device_id }
```

`event_id` identifies *this keypress sequence*. The application processes each
`event_id` once, using it as the command's `request_id`.

**Deduplication by barcode would be a bug.** Two scans of the same product mean
two units — the most ordinary thing a cashier does. What must not happen is one
physical scan becoming two items because the plumbing delivered it twice, and
that is exactly what the event id prevents.

`device_id` is opaque, so a second scanner later is a value rather than a
redesign. There is no device registry.

## Decision — a scan needs a sale

A scan with no editable sale open answers `SCAN_NO_ACTIVE_TRANSACTION`. PI.Edge
does **not** open one: no product decision says it should, and inventing a sale
because someone waved a barcode is the kind of helpfulness that produces
abandoned transactions. A scanner-specific code is used rather than the generic
one so a till can tell the two situations apart.

Resolution then goes through the ordinary `AddItem` use case — the same catalog
snapshot, the same promotions, the same basket-freeze rules. There is no
hardware-specific product lookup, and an unknown barcode answers
`PRODUCT_NOT_FOUND` exactly as a typed one does.

## Consequences

* The parser is pinned by unit tests covering CR, LF, CRLF, partial buffers,
  human typing, modifiers, and the same barcode twice.
* A scan is refused after the basket freezes, by the sale's rules and not by
  the scanner's.
* An Android host later implements the same capture and reuses the same parser.
