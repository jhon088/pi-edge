# ADR 024 — Business day identity, and forcing an end of day

Status: accepted (milestone 7)

## A business day is not a calendar date

A business day is an *operational* period: it opens when a store starts
trading and closes when someone closes it. It can span midnight, survive a
multi-day offline stretch, and outlive the date it started on.

So `business_day_id` is never derived from `DATE(now())`, and nothing in the
core compares a business day to today's date. One `OPEN` business day per
store, enforced by the storage layer, and a transaction, a session and a cash
movement all carry the id of the day they belong to rather than a timestamp
someone has to interpret.

## Closing normally

`CloseBusinessDay` refuses while any of these is true, and names which:

| Blocker | Why |
| --- | --- |
| an `OPEN` session exists | a drawer nobody has counted |
| a `CLOSE_REQUESTED` session exists | a count that was started and never declared |
| an active transaction exists | a sale in progress |
| an unresolved payment exists | money whose fate is unknown (ADR 018) |
| a pending safe drop exists | money whose location is unknown (ADR 023) |

These are reported together, as a list, so an operator sees everything standing
in the way instead of discovering them one refusal at a time.

## Forcing it

A store cannot be held open forever by a till nobody will come back to. A
central system, a supervisor or (later) a scheduler must be able to end the day.

**`ForceCloseBusinessDay` does not ignore state. It applies a policy to it.**

```
for each session still open or awaiting declaration:
      close it as FORCED, with closure_kind = FORCED
      declaration    = none            ← never fabricated
      declared_cash  = NULL
      expected_at_close = the ledger figure at this instant   ← recorded
      variance       = NULL             ← unknown, not zero
      journal SESSION_CLOSED { forced: true, reason, actor }
close the day, journal BUSINESS_DAY_FORCE_CLOSED { reason, actor, sessions[] }
```

Three rules make this safe:

1. **No counted cash is invented.** `declared_cash` stays `NULL` and the
   session is marked `UNDECLARED`. Writing `counted = expected` would
   manufacture a clean variance out of nothing — the single most damaging
   thing an automated end-of-day could do.
2. **A forced closure is distinguishable from a normal one** by a stored
   `closure_kind`, not by inferring it from a null field.
3. **Nothing is destroyed.** Transactions, payments, movements, declarations
   and pending approvals all survive. A forced day close ends a *period*; it
   does not resolve the exceptions inside it, and the exceptions remain
   queryable afterwards.

`reason` is mandatory and non-empty, and the actor is recorded. A forced close
with no stated reason is not an operational action, it is a loss of
information.

### The domain does not know who triggered it

`ForceCloseBusinessDay` takes an actor and a reason. Whether that actor is a
supervisor at the till, an operations console or a scheduler is the caller's
business. No cron, no timer and no clock-based trigger exists in the core, and
adding one later changes nothing here.

## Consequences

* A session closed by force has `expected_at_close` but no variance. Reports
  must treat "variance unknown" as a real category rather than zero.
* The natural follow-up is to reopen such a session (ADR 023), count it, and
  close it properly — which leaves both the forced closure and the real count
  in the record.
* Outbox: `RegisterSessionClosed` and `BusinessDayClosed` already exist and
  keep firing, forced closures included, with the closure kind in the payload.
  No new outbox event type is introduced, because no consumer needs one yet.
