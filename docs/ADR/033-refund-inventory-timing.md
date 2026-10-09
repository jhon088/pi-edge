# ADR 033 — When refunded goods come back

Status: accepted (milestone 9)

## Context

A card refund can sit unresolved for minutes. The merchandise, meanwhile, is
already on the counter. Two wrong answers are available:

* return the stock when the refund is *created* — before anyone accepted the
  return, so an abandoned refund silently inflates inventory;
* return it when the refund is financially *complete* — so a card refund whose
  answer was lost leaves goods that are physically in the store missing from the
  ledger, possibly for a long time.

The sale's lifecycle is not a template here. A sale takes money and *then* hands
over goods; a refund takes goods and *then* hands back money.

## Decision — commercial acceptance is its own fact

```
CREATED ──accept──> ACCEPTED ─────────────────────> COMPLETED
   │                    │                               ▲
   │                    │  REFUND_RETURN written here   │
  FAILED                └──> FINANCIAL_PENDING ─────────┘
                             (unresolved provider refund)
```

* **`CREATED`** — authorised, priced, and the quantities it names are reserved.
  Nothing physical or financial has happened, so this is the **only** state that
  can still fail.
* **`ACCEPTED`** — *the goods came back*. The inventory return is written here,
  in the same database transaction, exactly once. From here the commercial
  refund is irrevocable.
* **`FINANCIAL_PENDING`** — a provider refund is in flight or unknown. The goods
  stay returned. The way out is reconciling that same attempt, never a second
  refund (ADR 018).
* **`COMPLETED`** — accepted *and* financially settled.
* **`FAILED`** — terminal, reachable only from `CREATED`, and it releases the
  reservation. A refund that already took the goods never "fails"; it waits.

There is no `FINANCIAL_COMPLETED`: it would be `COMPLETED` under another name.
Five states, each with behaviour the others do not have — `ACCEPTED` can start a
refund payment, `FINANCIAL_PENDING` refuses to start another and demands
reconciliation.

## Decision — `REFUND_RETURN`, direction `IN`

A new inventory movement type, introduced now because it finally has an
operation (frozen in milestone 7). One movement per refund line, keyed
`{refund_id}:{line_number}`, so a replay or a crash-and-retry cannot return the
same goods twice.

**Not `MANUAL_IN`.** A manual movement says an operator put stock in; a refund
return says a customer brought it back. Collapsing them would destroy the one
distinction an auditor is looking for, and `MANUAL_IN` requires a reason code
that a refund does not have — its reason is the refund.

The quantity is the **physical** quantity, exactly as on the way out: a
buy-2-get-1 line that moved three units out returns three units, whatever the
promotion did to the money (ADR 027).

## Decision — cash leaves the drawer when the refund is paid

`CASH_REFUND`, direction `OUT`, one movement per cash refund payment, keyed by
`refund_payment_id`. Cash has no provider, so it settles inside the same
transaction that records the refund payment — exactly like a cash sale, in
reverse. Expected cash falls by it, once.

It is **not** a reversal of the original `CASH_SALE`: that movement stays
exactly as it was, and the drawer's history shows money in on one day and money
out on another, which is what happened.

## Decision — restock is a value, not an engine

V1 returns everything to sellable stock. `restock` is a column, set `true`,
recorded on every refund. A damaged-goods disposition would be another value
and a rule about which movement type to write — not a disposition subsystem,
and not today.

## Consequences

* There is no state in which a refund is accepted and the goods are not on the
  ledger, and none in which the goods are back and the refund is not accepted.
* A refund whose card leg is unknown shows: stock returned, money pending,
  needs reconciliation. All three facts, separately, truthfully.
* An abandoned refund (`FAILED` from `CREATED`) moves nothing and frees what it
  held.
