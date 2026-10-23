# ADR 043 — A receipt is an obligation, not a transaction state

Status: accepted (milestone 12)
Amends ADR 005 and the lifecycle fixed in milestone 1.

## Context

Milestone 1 gave `RetailTransaction` a `RECEIPT_COMPLETED` status between
`COMMITTED` and `FINALIZED`, assuming the order *pay → print → close*.
Milestone 10 froze the opposite (ADR 037): a physical side effect never
participates in a commercial transaction, and printing requires the sale to be
`FINALIZED` already. That made `RECEIPT_COMPLETED` topologically unreachable —
to enter it a sale had to be `COMMITTED`, but nothing could print until it was
`FINALIZED` — and nothing ever called the transition.

Meanwhile nothing in the core knew that a closed sale *owed* a receipt. Paper
came out only if a client asked for it, so a client that crashed, or simply
never asked, left a sale with no ticket and nothing anywhere saying so. And the
two ambiguous print outcomes were treated oppositely by accident: a crash after
the bytes went out left the device claim `PENDING` forever and the first receipt
permanently refused with `HARDWARE_ACTION_ALREADY_COMPLETED`, while a socket
error after a partial write was recorded as `FAILED` and freely retried — the
same physical uncertainty, two different answers.

## Decision — three truths, three places

```text
retail_transactions   FINALIZED   the sale is commercially closed
transaction_receipts  PENDING     …and owes a receipt
hardware_actions      PENDING     …and we touched a printer about it
```

**`FINALIZED` means the sale is commercially closed**: payments resolved, cash
already moved by the payment that moved it, stock moved, journal written,
`SALE_FINALIZED` written. It says nothing about paper, a confirmed receipt or a
healthy printer, and it never will.

`RECEIPT_COMPLETED` is **removed** from the status set (migration 0011). A row
left in it has no honest conversion — it is not `FINALIZED`, it has no
`finalized_at` — so the migration refuses to run rather than inventing one.

`transaction_receipts` is one row per sale, created **in the same transaction**
as `FINALIZED`. There is no state in which a sale is commercially closed and
nobody knows a ticket is owed. It is deliberately not `hardware_actions`: that
table records what was attempted on a device, and one sale can produce a failed
attempt, a retry and any number of reprints while still owing exactly one
receipt.

There is no `Receipt` aggregate. A row with four states and a transition rule
is the whole of it; an aggregate would be a bigger word for the same thing.

## Decision — the states are about certainty

```text
PENDING ──printed──> PRINTED   terminal
   │   \
   │    `──not sent──> FAILED  retryable: we know no paper came out
   │                     │
   │                     `──(explicit retry)
   `────uncertain────> UNKNOWN terminal: paper may have come out, and no
                               amount of retrying will tell us
```

Which one a failure produces depends on a single question — **did the printer
get the job?** — and only the adapter knows the answer, so the answer lives in
the error taxonomy rather than in a caller's guesswork:

| Physical fact | `HardwareError` | Receipt |
| --- | --- | --- |
| finished the job | — | `PRINTED` |
| nothing opened, or no driver | `Unavailable`, `NotConfigured` | `FAILED` |
| bytes went out, no confirmation | `Timeout`, `Io` | `UNKNOWN` |

One sharpening in M10's taxonomy follows: a **connection** that timed out is
now `Unavailable`, not `Timeout`. A socket that never opened carried no bytes,
and reporting it as a timeout made a printer we never reached look like one
that may have printed. `Timeout` keeps its M10 meaning — reached, given the
job, no answer — and still maps to `DEGRADED` in the device view.

`UNKNOWN` is terminal and cannot be overwritten, **not even by a later
success**. The question "did the customer get a ticket?" is closed, unanswered,
on purpose; a system that silently revised it would be claiming knowledge
nobody has.

## Decision — the core attempts the first receipt

After the commercial commit, outside every transaction, in the claim → call →
record shape payments and the drawer already use (ADR 020, ADR 037). Failure is
swallowed and recorded, exactly like the drawer: the sale is closed, the money
has moved, and no printer can un-move it.

A client that dies therefore cannot leave a sale without its ticket, and no
client decides when a receipt is issued.

`PRINT_TRANSACTION` keeps both of its jobs and gains a gate:

* `reprint: false` is an **explicit retry of the first receipt**, allowed only
  while the obligation says no paper came out — `RECEIPT_ALREADY_PRINTED` or
  `RECEIPT_OUTCOME_UNKNOWN` otherwise. `HARDWARE_ACTION_ALREADY_COMPLETED` is
  no longer used to express uncertainty, because it was never what it meant.
* `reprint: true` is unchanged from M10: explicit, repeatable, marked as a
  copy, and it changes nothing — no cash, no stock, no drawer, no sale, **and
  not the obligation**. A copy printed today says nothing about yesterday.

## Decision — recovery distinguishes two `PENDING`s

At startup, one deterministic pass. No watcher, no queue, no scheduler.

| Durable evidence | What it means | What recovery does |
| --- | --- | --- |
| no device action at all | the process died before the claim; nothing was sent | attempt it now |
| a claim with no resolution | the printer may already have the job | `UNKNOWN`, print nothing |
| a resolved claim | the device answered and the crash beat the update | apply that answer |

Confusing the first two is how a till prints a second ticket for a customer who
already has one, or silently loses a first one.

## Decision — a receipt never blocks a close

A session and a business day close with receipts `PENDING`, `FAILED` or
`UNKNOWN`. A receipt is an operational obligation, not a financial one: no
money is unaccounted for because a printer jammed.

Both closes *report* the counts in their own result — `SessionCloseResult` and
`BusinessDayCloseResult { day, receipt_attention }` — so a caller never has to
run a second query to learn, at the moment it closes the day, that a receipt is
outstanding. `PRINTED` is not among the counts: it is not an attention item.
The forced close answers the same shape, because a day ended over open sessions
is still a day close and should report what needs somebody just as loudly.

Each count is scoped to what that close is actually responsible for, through
the sale's own identity rather than a window of time:

| Consumer | Scope | Answers |
| --- | --- | --- |
| `SessionCloseResult` | `RegisterSession(session_id)` | what *this till* still owes |
| `BusinessDayCloseResult` | `BusinessDay(business_day_id)` | what *this day* still owes |
| the operational history | `Global` | what the installation still owes |

Closing one till must not report another till's unresolved receipt, even on the
same day, and closing a day must not report another day's. Both identities are
already on `RetailTransaction`, so one query expresses all three scopes.

`BusinessDay` itself learns nothing about any of this: the attention is computed
by the application and carried in the result DTO. The aggregate knows no
hardware and no receipts.

## Decision — cancelling a sale

`CANCEL_TRANSACTION` exists, using the `cancel` the aggregate has had since
milestone 1 and that no use case ever called. Allowed from `OPEN` and
`TOTALIZED` only: once a payment is applied the way out is a reversal or a
refund (ADR 019). Nothing moves — no cash, no stock, no outbox fact, no receipt
obligation — because nothing happened.

It is not a convenience. Without it an abandoned sale blocks the session close
forever, with no way out that does not involve money.

## Decision — a minimal operational history

`LIST_RECENT_SALES`: finalized sales, newest first, bounded, each with its own
materialised total and its receipt state. Enough for an operator — and for the
productive UI of the next milestone — to find the sale just made and reprint
it.

Not reporting: no analytics, no search, no filters, no aggregates, and no money
recomputed. The tender breakdown is one existing call away and is not
duplicated here.

## Consequences

* Finalizing a sale now touches the printer. The call is outside the
  transaction and its failure is swallowed, so the only observable difference
  to a client is that the ticket arrives without being asked for.
* A till with no printer configured records every receipt as `FAILED`, which is
  honest: nothing was sent, and the attention list says so.
* There is no refund receipt lifecycle. Nothing in the product requires a
  printed refund voucher today, and inventing a second lifecycle for symmetry
  would be building what was not asked for. A refund can be reprinted as part
  of the sale it refunds; a dedicated refund document is a future product
  decision.
