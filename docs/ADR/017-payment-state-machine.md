# ADR 017 — The payment state machine

Status: accepted (milestone 6)

## Context

A payment lifecycle has to answer four operational questions at any instant:
is the money applied, is the basket locked, can the sale be finalized, and
does something need reconciling. Those answers must come from the stored state,
not from inference.

## Decision

Two state machines, one per concept (ADR 016).

### Payment

```
CREATED ──> IN_PROGRESS ──> COMMITTED
   │             │
   │             ├────────> FAILED        (terminal)
   │             │
   └─────────────┴────────> REVERSED      (terminal)

CASH:  CREATED ──> COMMITTED              (one transaction, no attempt)
COMMITTED ──> REVERSED                    (via a reversal, ADR 019)
```

| State | Money applied | Blocks basket | Blocks finalize | Needs reconciliation |
| --- | --- | --- | --- | --- |
| `CREATED` | no | **yes** | **yes** | no |
| `IN_PROGRESS` | no | **yes** | **yes** | depends on its attempts |
| `COMMITTED` | **yes** | **yes** | no | no |
| `FAILED` | no | no | no | no |
| `REVERSED` | no | no | no | no |

`CREATED` blocks: it exists precisely because the record is written *before*
the provider is called (ADR 020), so a `CREATED` payment may already have
reached a provider.

### PaymentAttempt

```
CREATED ──> PROCESSING ──┬──> AUTHORIZED ──> REVERSAL_PENDING ──> REVERSED
                         │
                         ├──> FAILED                               (terminal)
                         │
                         └──> UNKNOWN ──┬──> AUTHORIZED
                                        └──> FAILED
```

| State | Terminal | Blocks basket | Blocks finalize | Needs reconciliation |
| --- | --- | --- | --- | --- |
| `CREATED` | no | yes | yes | no |
| `PROCESSING` | no | yes | **yes** | **yes** |
| `AUTHORIZED` | no | yes | no | no |
| `UNKNOWN` | no | yes | **yes** | **yes** |
| `REVERSAL_PENDING` | no | yes | **yes** | **yes** |
| `FAILED` | **yes** | no | no | no |
| `REVERSED` | **yes** | no | no | no |

`PROCESSING` is treated as *unresolved and requiring reconciliation*, not as a
transient internal flag. A process that dies between the provider call and the
result therefore needs **no startup repair**: the stored state already says
"we asked and we do not know", which is the truth. Nothing ever promotes
`PROCESSING` to `FAILED`.

### Invalid transitions

Every transition not drawn above is refused with `PAYMENT_INVALID_STATE`.
In particular: no `FAILED → *`, no `REVERSED → *`, no `AUTHORIZED → FAILED`
(that is a reversal, not a failure), no `UNKNOWN → UNKNOWN` from a second
provider call, and no `CREATED → AUTHORIZED` skipping `PROCESSING`.

Resolving an attempt that is already in the state the provider reports is a
no-op, not an error: that is what makes a retried reconcile safe.

## Consequences

* The four operational questions are pure functions over the stored states —
  see `edge_payments::settlement`.
* Adding a tender adds rows, not states.
* `REVERSAL_PENDING` blocks finalization: a reversal we started but could not
  confirm is unfinished financial business.
