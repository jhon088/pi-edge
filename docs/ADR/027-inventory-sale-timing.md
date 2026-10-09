# ADR 027 — When a sale moves stock

Status: accepted (milestone 8)

## Context

A sale passes through `OPEN → TOTALIZED → PAYMENT_IN_PROGRESS → COMMITTED →
FINALIZED`, and any of those steps can still fail or be reversed. Moving stock
at the wrong one means the ledger claims goods left the store when they did
not.

## Decision

**Stock moves at `FINALIZED`, and nowhere else.**

| Moment | Moves stock |
| --- | --- |
| adding a line to the basket | no |
| totalizing | no |
| starting a payment | no |
| a card authorization | no |
| `COMMITTED` (money applied) | no |
| **`FINALIZED`** | **yes** |

Everything before `FINALIZED` is still reversible: a payment can be declined,
come back `UNKNOWN`, or be reversed (ADR 019), and the basket can be edited
again if every payment ends up failed or reversed. Stock that moved for such a
sale would have to move back, and a compensating movement for a sale that never
happened is noise in the only record an operator can trust.

`RECEIPT_COMPLETED` exists in the aggregate but is optional and no use case
drives it, so it is not a competing moment: it sits *before* finalization, not
after.

One movement per line, written **inside the same database transaction** as the
finalization, its journal event and its outbox event. There is no state in
which a sale is `FINALIZED` and its stock has not moved, and none in which
stock moved for a sale that is not finalized.

### Physical quantity, not billed quantity

The movement carries `TransactionLine.quantity` — what physically left the
store. A buy-2-get-1 line is `quantity = 3.000` with a discount on the money
(frozen in milestone 5: a promotion may only produce a per-line discount
amount), so three units leave and the ledger says three. Inventory never
re-reads a promotion; it consumes the materialised transaction.

Fractional quantities work because it is the same `Quantity` type: `0.750 kg`
sold is `0.750` out.

### Idempotency

`source_ref` is `"{transaction_id}:{line_number}"`, unique per
`(store_id, movement_type, source_ref)`. A replayed `FinalizeTransaction`
cannot write a second movement for a line — the database refuses it, and the
service checks first so a replay is a clean no-op rather than an error.

Command idempotency (`request_id` in `processed_commands`) and source identity
(`source_ref`) stay separate concepts: the first makes a retried *call*
harmless, the second makes a duplicated *fact* impossible.

## Consequences

* A sale abandoned at any point before finalization leaves the ledger
  untouched, with nothing to clean up.
* Refund inventory is out of scope: returning goods is a different commercial
  document and arrives with its own workflow and movement type.
