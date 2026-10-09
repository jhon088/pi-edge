# ADR 035 — The hardware boundary, and what a device's state means

Status: accepted (milestone 10)

## Context

ADR 005 settled that hardware sits behind ports injected at the composition
root, and its milestone-7 amendment settled *when* the ports get written: with
the first device we actually talk to. `edge-hardware-api` had shipped as a crate
of traits nobody implemented and was deleted for it.

This is that milestone. The ports arrive with adapters, and the question this
ADR answers is where the boundary runs and what the diagnostic surface may say.

## Decision — one crate, ports with adapters

`crates/edge-hardware` holds the device model, the scan parser, `PrinterPort`,
`CashDrawerPort`, the ESC/POS renderer and transport, and simulators. It knows
devices and nothing else: no POS domain, no money, no catalog, no SQLite, no
IPC, no Tauri, no Android. The only I/O in it is one `std::net::TcpStream` in
the ESC/POS adapter.

Three device kinds, because three are driven. There is no `CustomerDisplayPort`
and no `PaymentTerminalPort`: both were in the deleted crate, neither has an
adapter, and writing them again would repeat the mistake.

**There is no `ScannerPort`.** A scanner is *input*, not a capability the core
invokes, so there is nothing to call. What exists is the parser and the event
(ADR 036).

## Decision — hardware is injected, not parameterised

```rust
Hardware { printer: Arc<dyn PrinterPort>, drawer: Arc<dyn CashDrawerPort>, paper_width }
```

`EdgeApplication` already carries four type parameters. Three more would ripple
through every test harness of nine milestones to buy static dispatch on calls
that take milliseconds of I/O and happen outside every database transaction.
Trait objects here cost nothing measurable and keep frozen code frozen.

## Decision — four states, and `UNKNOWN` is the default

| State | Meaning |
| --- | --- |
| `AVAILABLE` | reached, and it worked |
| `UNAVAILABLE` | configured, and could not be reached or it refused |
| `DEGRADED` | reached, and the outcome is **unresolved** — a timeout, so the effect may or may not have happened |
| `UNKNOWN` | never probed in this process lifetime |

Two rules follow, and both are tested:

* **Configured is not available.** A device nobody has touched is `UNKNOWN`. A
  POS that claims a printer works because an address is set in a file will
  discover otherwise in front of a customer.
* **A restart returns every device to `UNKNOWN`.** An `AVAILABLE` recorded
  before the process died says nothing about now, so the current state is
  process memory and is never persisted as if it were still true.

What *is* durable is what a device **did**: `last_success_at` is rebuilt from
the hardware action log, so a diagnostic screen can say "it last worked at
14:02" after a restart while refusing to say "it works".

A timeout is `DEGRADED` rather than `UNAVAILABLE` on purpose. "We asked and we
do not know" is a different fact from "it is not there", and for a printer it is
the difference between a receipt that exists and one that does not.

## Decision — the diagnostic surface discloses nothing

`HardwareStatusV1` carries a kind, an opaque device id, a **driver name**, a
state, timestamps and an error code. It does not carry a host, a port, a serial
number, a stack trace or a payload, and a test asserts the rendered status
contains neither the configured address nor the port.

A diagnostic view is read by whoever is standing at the till. It must not double
as a way to read the installation's network configuration (ADR 039).

## Consequences

* Supporting a new device is a new adapter plus a line of configuration.
* The whole POS runs, is tested and is demonstrated with nothing plugged in.
* `edge-hardware` is the only crate that may ever contain an escape sequence.
