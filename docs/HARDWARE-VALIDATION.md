# Hardware validation

Two kinds of gate, kept apart on purpose. Software gates run anywhere and are
part of CI. **Physical gates need the device in the room**, and nothing marks
one as passed without it.

## Matrix

| Capability | Software verified | Physically verified | Hardware / topology |
| --- | --- | --- | --- |
| Scanner, keyboard wedge | **yes** — parser (CR, LF, CRLF, partial, human typing, modifiers, repeat), scan → one item, event replay, no-sale refusal, basket freeze, over IPC and through the CLI | **no** | USB HID scanner in wedge mode → focused window → Tauri host → parser |
| ESC/POS TCP printer | **yes** — deterministic renderer, mode resets, cut, non-ASCII folding, unreachable host, unresolved address, failure not changing a sale, reprint | **no** | 80 mm thermal printer on the LAN, raw TCP (9100 by convention) |
| ESC/POS drawer kick | **yes** — `DLE DC4` pulse bytes, driver selection, refusal to borrow a simulated printer, idempotency, failure not changing a sale | **no** | drawer on the printer's DK port |
| iMin direct drawer | **partially** — the port, the driver selection, the refusal path and the error semantics. The bridge itself is a declared integration point, not an implementation | **no** — in the *previous* POS this path was physically validated (`IminSDKManager.opencashBox`); that is a historical baseline and says nothing about PI.Edge | drawer built into an iMin terminal, via Android host → Kotlin → iMin SDK |

Read the last row carefully. `EscPosKickDrawer` running does **not** close the
iMin gate: they are different devices on different wires (ADR 038).

## Software gates

Run everywhere, no hardware:

```bash
cargo test --workspace          # includes every simulator and the parser
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Devices are selected by configuration, read once in the composition root:

```text
PI_EDGE_PRINTER=simulator[:works|unavailable|timeout|io_error]
PI_EDGE_PRINTER=escpos_tcp:<host>[:<port>]
PI_EDGE_DRAWER=simulator[:outcome] | escpos_kick[:<host>[:<port>]] | imin
PI_EDGE_PAPER_WIDTH=48
```

An unrecognised value refuses to start. **A missing one, or `none`, configures
no device at all**: every call answers `HARDWARE_NOT_CONFIGURED` and the
diagnostic view reports `driver=unconfigured`. A simulator is opt-in, so a
misconfigured runtime can never report `AVAILABLE` on fake hardware (ADR 038).
Nothing falls back from one real driver to another either.

## Physical gates — the Swan session

Nothing below has been run. Record PASS/FAIL per case, with the device, the
firmware and the date.

### A. Scanner, USB HID

| # | Case | Expected |
| --- | --- | --- |
| A1 | Open a sale, scan a known SKU ten times | ten units, no doubles, no missed scans |
| A2 | Scan two different SKUs alternately, ten times | ten lines/units as scanned, nothing interleaved wrongly |
| A3 | Type a barcode by hand on the keyboard and press Enter | **no** scan; human typing is not a scan |
| A4 | Start a scan, walk away mid-barcode, scan again | the partial is discarded; the second scan is correct |
| A5 | Scan with no sale open | `SCAN_NO_ACTIVE_TRANSACTION`; no sale is created |
| A6 | Scan after taking payment | `BASKET_LOCKED_BY_PAYMENT` |
| A7 | Unplug the scanner, plug it back in, scan | works again with no restart of the POS |
| A8 | Scan an unknown barcode | `PRODUCT_NOT_FOUND`; the sale is unchanged |

### B. Printer, Ethernet ESC/POS 80 mm

Configure `PI_EDGE_PRINTER=escpos_tcp:<the printer's IP>:9100`.

| # | Case | Expected |
| --- | --- | --- |
| B1 | `edge-cli printer-test` | a legible page, correct width, clean cut |
| B2 | Accented and `ñ` text on the page | folded to base letters, no garbage bytes |
| B3 | `edge-cli print-transaction --transaction <id>` | receipt with ticket, lines, discounts, totals |
| B4 | `--reprint` | the same receipt with `** COPIA **` |
| B5 | Second automatic receipt for the same sale | `HARDWARE_ACTION_ALREADY_COMPLETED` |
| B6 | Pull the network cable, print | `HARDWARE_UNAVAILABLE`; the sale stays `FINALIZED`, the cash and stock ledgers unchanged |
| B7 | Reconnect, print again | works; no restart needed |
| B8 | Power the printer off mid-print | `HARDWARE_TIMEOUT` or `HARDWARE_IO_ERROR`; device reports `DEGRADED`/`UNAVAILABLE`; nothing commercial changes |
| B9 | `hardware-status` after each of the above | state and `last_error_code` match what happened; **no host or port in the response** |

### C. Cash drawer

Run the whole block **twice**: once with `PI_EDGE_DRAWER=escpos_kick` (drawer on
the printer) and once with `PI_EDGE_DRAWER=imin` on the Swan (drawer in the
terminal). The second run requires the Android bridge, which does not exist yet
— so it is blocked until that is built, and must not be reported as passed
before then.

| # | Case | Expected |
| --- | --- | --- |
| C1 | `edge-cli drawer-open` | opens |
| C2 | `drawer-open` three times | opens three times |
| C3 | Cash sale | opens once |
| C4 | Replay the same `--request-id` payment | does **not** open again |
| C5 | Card-only sale | does **not** open |
| C6 | Mixed tender with cash | opens once |
| C7 | Reprint a receipt | does **not** open |
| C8 | Cash refund | opens once |
| C9 | Card refund | does **not** open |
| C10 | Disconnect the drawer, take a cash sale | does not open; the sale stays paid, expected cash is still right, the failure is in `hardware-status` |
| C11 | Reconnect, `drawer-open` | opens; a failed action never blocks a retry |
| C12 | Restart the daemon, replay a payment that already opened it | does **not** open again |
| C13 | With `PI_EDGE_DRAWER=imin` and no bridge | `HARDWARE_NOT_CONFIGURED`, and **no** ESC/POS pulse is sent |

### D. Swan, end to end

| # | Case | Expected |
| --- | --- | --- |
| D1 | Sale: scan 3 items, cash, print, drawer | one receipt, one opening, correct totals |
| D2 | Day of trading, ~50 sales | no duplicate scans, no duplicate openings, no stuck `PENDING` actions |
| D3 | Pull power mid-sale | on restart: no half-finalized sale, no phantom drawer opening, devices back to `UNKNOWN` until probed |

Performance on the Swan is **not** claimed until D2 has been run.
