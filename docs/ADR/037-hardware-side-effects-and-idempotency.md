# ADR 037 — Physical side effects, and how not to do them twice

Status: accepted (milestone 10)

## Context

Two things can go wrong with a physical side effect, and they pull in opposite
directions.

A drawer that opens **twice** because a command was replayed leaves cash exposed
with nobody expecting it. A sale that is **rolled back** because a printer jammed
takes money that was paid and goods that left and pretends neither happened.

ADR 005 already settled the second: hardware failure is never a domain failure.
This ADR settles the first and makes the second operational.

## Decision — the device is touched outside every transaction

```text
1. claim the action      (database transaction)
2. call the device       (no transaction open)
3. record what it said   (database transaction)
```

Three interactions, on purpose. A slow printer cannot hold a write lock, and a
failing one has nothing to roll back because nothing is open. This is the
bracket payments already use around a provider call (ADR 020), applied to paper
and solenoids.

The claim comes **first**. A process that dies between the claim and the device
leaves a `PENDING` row — evidence that the drawer may already be open. The safe
answer to "did it happen?" is yes, so `PENDING` blocks a repeat exactly as
`COMPLETED` does. An operator can always open the drawer from the diagnostic
surface, which is deliberate and audited; what must never happen is a drawer
opening twice because a command was retried.

A `FAILED` action does **not** block a retry: the device was down, nothing
happened, and trying again is correct.

## Decision — the log, and what it is not

```sql
hardware_actions (action_id, kind, device_kind, reason, source_ref, status,
                  error_code, error_message, requested_at, completed_at)

UNIQUE (kind, source_ref) WHERE source_ref IS NOT NULL
                            AND status IN ('PENDING', 'COMPLETED')
```

One table, append-only, enforced by a trigger. It exists to stop a repeat and to
answer "what did this device last do". It is **not** a print queue, not a job
scheduler, not an event store and not a receipt. There is no `PrintJob`
aggregate: the source of a reprint is the sale, and the commercial and fiscal
lifecycle of a receipt is its own milestone (ADR 039).

## Decision — commercial causes are keyed; diagnostics are not

| Action | `source_ref` | Repeats? |
| --- | --- | --- |
| drawer, cash sale | the payment id | no |
| drawer, cash refund | the refund payment id | no |
| drawer, diagnostic | none | **yes, every time asked** |
| first receipt | the transaction id | no |
| reprint | none | **yes, every time asked** |
| test page | none | yes |

The distinction is the whole design. An automatic action derived from a
commercial event must happen once per event, so it is keyed by that event. An
operator pressing a button is not a replay, and refusing the second press would
make the button useless — so a diagnostic is unkeyed and every opening is
audited.

Asking for the *first* receipt twice answers
`HARDWARE_ACTION_ALREADY_COMPLETED`: a second copy is a reprint and has to say
so, which keeps "how many receipts were printed automatically" answerable.

## Decision — a failed side effect never changes commercial truth

`CashMovement` is durable, *then* the drawer is asked to open. If it does not:

* the sale stays paid and `FINALIZED`;
* the cash ledger still says what should be in the drawer — expected cash is
  what the ledger derives, not what a solenoid did;
* the refund stays `COMPLETED` and the goods stay returned;
* the failure is recorded against the device and visible in diagnostics.

The same for paper. A printer that is down, times out or breaks mid-write leaves
the sale finalized, the stock moved and the drawer ledger right. Tests assert all
three for all three failure modes.

**A reprint moves nothing**: no cash, no stock, no drawer, no change to the sale.
That was true in the previous POS and it is now a test.

## Decision — retry policy, by device

* **Printer**: retry is a human decision — a reprint. Nothing retries
  automatically, because a timeout may mean the paper already came out.
* **Drawer**: never retried automatically. A loop here opens the drawer
  repeatedly, which is the failure mode this ADR exists to prevent.
* **Scanner**: input. There is nothing to retry.

## Consequences

* A simulator's open count is the test surface for every rule above, and a
  restart gives a fresh count — so whatever prevents a second opening has to be
  durable, which is the point.
* `PENDING` can strand one automatic opening after an unlucky crash. That is the
  deliberate direction of the trade: a drawer that did not open is an
  inconvenience, and one that opened unattended is a loss.
