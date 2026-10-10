# ADR 042 — A refund is authorised online, for one operation

Status: accepted (milestone 11)
Amends ADR 030.

## Context

ADR 030 said a production refund needs central authority over the original sale,
and left a development stand-in (`PI_EDGE_REFUND_AUTHORITY=dev-accept-local`)
because there was no server to ask. Milestone 11 gives the composition root a
transport it *can* ask.

That raises an obvious-looking temptation. Once a sale has been centrally
accepted for a refund, why not remember it? A `sale_refund_authorized` flag, or
a `central_sale_accepted` projection, would let a later refund of the same sale
proceed with no server — which looks like offline resilience and reads, in the
ledger, like an authorisation nobody gave.

## Decision — no durable grant

**Production refunds are online.** Creating a refund requires, at that moment:

1. a lookup of the original sale against the remote authority,
2. central validation and authorisation *now*,
3. a `RefundAuthorizationContext` with `CENTRAL_ACCEPTED` for **that**
   operation,
4. and only then is the `Refund` created.

The answer may be materialised **inside the refund** as evidence —
`authorization_ref` — because evidence of what was authorised belongs with the
thing it authorised. It does **not** make the sale "authorised for future
refunds". There is no `sale_refund_authorized`, no reusable projection, no cache
of past decisions, and no equivalent by another name.

**PI.Edge sells offline; it does not refund offline.** That asymmetry is
deliberate and it is a product rule, not a limitation of the implementation: a
sale creates an obligation the business can still reconcile later, and a refund
hands money out of the drawer against a sale the till cannot verify.

## Decision — three refusals, three codes

All three are refusals, and an operator does something different about each, so
they are not collapsed into one code:

| Situation | Code | What to do about it |
| --- | --- | --- |
| no authority configured | `REFUND_AUTHORITY_REQUIRED` | configure the installation |
| configured, unreachable or timed out | `REFUND_AUTHORITY_UNAVAILABLE` | try again shortly |
| reached, and it said no | `REFUND_NOT_AUTHORIZED` | nothing; the decision was made |

The transport contract distinguishes them at the source:
`authorize_refund` answers `Accepted`, `Refused { reason }`, or a `SyncError`
(unavailable, timeout, malformed). The authority's own reason travels with
`REFUND_NOT_AUTHORIZED`, because it is the only part of the refusal the operator
cannot work out for themselves. None of this invents HTTP or a server.

## Decision — the resolver, and the three sources it has

The composition root resolves authority; the refund domain receives an answer
and asks no questions about where it came from (ADR 030, unchanged).

| Source | When | What it means |
| --- | --- | --- |
| `Transport` | a sync transport is configured | ask the authority, now, through `SyncTransport::authorize_refund` |
| `DevAcceptLocal` | `PI_EDGE_REFUND_AUTHORITY=dev-accept-local` | the development stand-in, marking every context `DEV-LOCAL:` |
| `Unavailable` | neither | every refund refused |

An explicit development switch wins when it is set, because explicit
configuration beats inference. With neither, the production rule is the default.

`authorize_refund` is added to `SyncTransport` **now** because it has a real
consumer now — the resolver calls it, and the simulator answers it in tests. It
is not a method waiting for a boundary. The request carries the transaction, the
tenant, the store and a per-question `request_id`; nothing about it is stored.

## Consequences

* A standalone installation with no server and no development switch cannot
  refund. That is correct, and it is visible: `REFUND_AUTHORITY_REQUIRED`.
* Asking twice about the same sale is two questions. Nothing short-circuits the
  second, which is the point.
* When HTTP exists, `authorize_refund` is the only refund-related thing it has
  to implement. The rest of the refund engine does not change.
