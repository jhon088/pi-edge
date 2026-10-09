# ADR 038 — The iMin drawer, and why nothing falls back

Status: accepted (milestone 10)

## Context

Two different topologies connect a cash drawer, and we have one of each in
front of us:

* **Drawer on the printer.** Thermal printers have a DK port; the drawer is a
  solenoid on the end of that wire and opens on an ESC/POS pulse. Since we
  already speak ESC/POS over TCP, this adapter is real and testable now.
* **Drawer in the terminal.** An iMin device has its own drawer, opened through
  the vendor SDK. The previous POS validated this **physically**:
  `Flutter → MethodChannel pi_commerce/imin_cashbox → Kotlin → IminSDKManager.opencashBox`.

The temptation is to treat the first as "the drawer works". It is not the same
device, the same wire or the same code path.

## Decision — three drivers, chosen explicitly

```text
CashDrawerPort
├── SimulatorDrawer    counts openings; what tests and demos use
├── EscPosKickDrawer   real: DLE DC4 pulse down the printer's wire
└── IminDrawer         real path: Android host → Kotlin → IminSDKManager.opencashBox
```

Selected by configuration, never inferred:

```text
PI_EDGE_DRAWER=simulator | escpos_kick | imin | none     (absent == none)
```

## Decision — a simulator is opt-in

**Hardware simulators are explicit development and test adapters. Missing
hardware configuration never selects a simulator implicitly.**

An absent `PI_EDGE_PRINTER` or `PI_EDGE_DRAWER` — or an explicit `none` —
selects `UnconfiguredPrinter` / `UnconfiguredDrawer`, which answer
`HARDWARE_NOT_CONFIGURED` to every call and report `driver=unconfigured`.

The failure mode this closes: a production runtime that was misconfigured would
otherwise come up reporting `AVAILABLE` on a `simulator` nobody asked for,
accept a print, record a successful drawer opening, and tell everyone the
hardware is healthy. Nobody finds out until a customer is waiting for a receipt
that was never going to exist.

So a till with nothing configured still **starts** — its diagnostics have to be
readable — and says plainly that it has no devices. A simulator is still
first-class for tests, demos and development; it is simply asked for:

```text
PI_EDGE_PRINTER=simulator
PI_EDGE_DRAWER=simulator
```

Test harnesses pass `Hardware::simulated()` explicitly for the same reason: a
test that runs on fake hardware says so out loud.

## Decision — no silent fallback, ever

If `imin` is configured and this host installed no bridge, every call answers
`HARDWARE_NOT_CONFIGURED` and says why. It does **not** try the ESC/POS pulse,
even when a printer is configured and would work.

A configuration that asks for the terminal's drawer and silently gets a pulse to
a different drawer is worse than one that refuses: the operator believes a
device is working, and discovers otherwise with a customer waiting. A refusal is
visible and fixable.

The same rule guards the other direction: `escpos_kick` refuses to borrow a
*simulated* printer's address, because "the drawer opened" would then be a lie.
And no configured device ever implies another: a configured printer does not
give you a drawer.

## Decision — the bridge is structural, and labelled

`IminDrawer` takes an optional closure the Android host installs, reaching
Kotlin and `IminSDKManager.opencashBox`. That closure is the integration point
and the only thing M10 leaves for the device.

What this buys: the port, the configuration, the driver selection, the error
semantics, the idempotency and the action log are all real and reviewable before
the hardware is in the room. What it does **not** buy, and must not be read as:
any claim that the iMin drawer has been exercised from PI.Edge.

No Android target is initialised and no SDK is linked. Doing either blind, with
no device to run it on, would produce a build nobody can verify and a stub that
looks like progress.

## Decision — physical validation is a separate gate

| Capability | Software verified | Physically verified |
| --- | --- | --- |
| ESC/POS TCP printer | yes | **pending** — needs the 80mm printer on the network |
| ESC/POS drawer kick | yes | **pending** — needs a drawer on that printer's DK port |
| iMin direct drawer | the port, config and refusal path | **no** — baseline exists in the *previous* POS; not yet in PI.Edge |

That `EscPosKickDrawer` runs does not close the iMin gate. The two are different
devices.

## Consequences

* A Swan session is required to turn either "pending" into a pass, and the plan
  for it is written down rather than improvised.
* Supporting a third topology is a fourth driver and a line of configuration.
* The legacy `MethodChannel` design is not copied; what is preserved is the
  knowledge of which SDK call opens the drawer.
