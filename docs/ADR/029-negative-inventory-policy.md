# ADR 029 — Negative stock, and where availability is checked

Status: accepted (milestone 8)

## Context

"Refuse the sale when stock is insufficient" sounds obviously right and is
obviously wrong for retail. A till is offline, its position is a day old, a
count was mistyped, a delivery was received but not entered. Blocking sales on
a number the store knows is stale turns a bookkeeping problem into lost revenue
at the counter.

The opposite — never checking — is also a policy, and a defensible one, but it
has to be chosen rather than assumed.

## Decision — an explicit, per-installation policy

`allow_negative_inventory` on the device profile, set at `InitDevice`. One
boolean, no policy framework: an installation serves one store, and that is the
granularity at which the decision is actually made.

| Setting | Behaviour |
| --- | --- |
| `true` | availability is never checked; `on_hand` may go negative |
| `false` | the start of payment is blocked when **this installation's local projection** says existence is insufficient |

### What `false` does and does not promise

`allow_negative_inventory = false` means exactly:

> block the start of payment when the local projection known to **this** Edge
> installation says existence is insufficient.

It does **not** mean:

> guarantee that the store physically holds enough.

PI.Edge is one POS. Its projection is `last known baseline + its own movements`
(ADR 026), so when other registers in the same store are selling offline, this
installation's figure is behind by whatever they have sold. A till can
therefore pass the check and still be the second one to sell the last unit.

**This is a known distributed limitation, not a bug.** Closing it needs
connectivity with PI.COMMERCE Server, a store-level Store Hub, or an explicit
coordination/reservation strategy — none of which is in scope here (ADR 026).
What the flag buys today is a real, useful guard against the single-register
case and against obvious oversell, enforced before any money moves.

## Decision — the check goes before the money, not after

With `allow_negative = false`, availability is checked at **`StartPayment`**:
the last moment at which nothing has been charged and refusing costs the
customer nothing but a re-scan.

At `FINALIZED` the movements are **recorded unconditionally**, even if that
drives `on_hand` negative.

This is the part worth being explicit about. Blocking at finalization would
mean: the goods have left the counter, the customer has paid, and the system
refuses to write it down. The stock is gone either way — refusing the movement
does not bring it back, it only makes the ledger disagree with reality, and it
strands a paid sale that cannot be closed (a cash payment cannot even be
reversed, see milestone 6). After money moves, the ledger's job is to record
what happened, not to enforce a policy that can no longer change the outcome.

So the policy lives where it can still change something, and the ledger stays
truthful. A negative `on_hand` is a real, visible, reportable condition — which
is the correct output for "we sold something the system did not know we had".

### The windows, and why they stay open

There are two, and they are different sizes.

**Within one installation**: between the check at `StartPayment` and the
movement at finalization, another operation on the same till — a manual out,
say — can move stock. Narrow, and visible in the ledger afterwards.

**Across installations**: another POS in the same store can sell the same unit
at any time, and this installation will not know until a consolidation happens.
That window is as wide as the disconnection, and no local check can narrow it.

**No reservations** (and a local one would not help the second case anyway): a
single-register offline POS has no concurrent basket to race against, and a
reservation engine brings expiry, release-on-abandon, and recovery of
reservations orphaned by a crash — a whole subsystem to defend an invariant
that nothing real is currently violating. Store-wide reservations are not a
local feature at all: they need the coordination point ADR 026 lists. Both are
documented here and neither is built.

## Consequences

* `allow_negative = false` is enforceable without reservations, at the cost of
  windows that are reported rather than prevented — and it is a statement about
  this installation's projection, not about the store's shelves.
* `allow_negative = true` is the offline-tolerant default a desynchronised
  store wants, and it never blocks a sale.
* Either way, the ledger always reflects what left **this** counter.
