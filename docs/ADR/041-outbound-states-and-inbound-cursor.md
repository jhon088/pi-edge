# ADR 041 — What happens to a fact, and how far we have read

Status: accepted (milestone 11)
Amends ADR 004 and ADR 028.

## Context

ADR 004 fixed `PENDING -> SENDING -> SENT` before there was a dispatcher. Three
questions only appear once something actually dispatches:

1. What happens to a fact the server will *never* accept?
2. What happens to a fact that was in flight when the process died?
3. How does an installation remember how far it has consumed the inbound
   stream, without ever claiming to have applied something it has not?

## Decision — `FAILED` is a state, and it is terminal

Status becomes `PENDING | SENDING | SENT | FAILED`.

A batch is a unit of **transport**, never a unit of truth: every fact in it is
answered on its own, and one answer maps to exactly one next state.

| The server says | The fact becomes |
| --- | --- |
| acknowledged | `SENT` |
| transient failure | `PENDING`, attempt counted, `next_attempt_at` per backoff |
| permanent failure | `FAILED`, with the reason attached |

`FAILED` is terminal, visible and **never retried automatically**. Retrying
forever a fact the consumer has rejected on contract grounds is a lie told to an
operator; the honest answer is to stop, say so, and wait for a person. Nothing
deletes it: migration 0010's trigger covers `FAILED` exactly as it covers
`PENDING`.

An aggregate with a `FAILED` fact is skipped until somebody intervenes, because
handing the consumer version `n+1` of an aggregate whose version `n` was
rejected would give it a history with a hole in it. Every *other* aggregate
keeps flowing: one terminal fact must never stop a till's unrelated sales from
syncing.

**Ordering is `seq`, plus one rule.** `seq` is SQLite's own autoincrement and
therefore the installation's durable order, so two runs over the same pending
set form the same batch. On top of that, at most one fact per aggregate travels
per batch, which is what guarantees version `n+1` is never in flight before `n`
is acknowledged. That is the whole ordering guarantee, built from metadata the
outbox already carries. There is no dependency graph and there should never be
one.

**Retention is deferred.** Milestone 11 purges nothing — `PENDING`, `SENDING`,
`SENT` and `FAILED` are all kept. A retention policy is a decision to make when
there is a real operational need, with real numbers; inventing one now would be
guessing.

## Decision — `SENDING` is not a resting state

A fact is claimed (`PENDING -> SENDING`, attempt counted) in its own transaction
*before* the transport is called, which is ADR 020's rule applied to sync: the
attempt is durable before the remote is contacted, so a process that dies
mid-call leaves evidence that the facts may already have landed.

**At startup, every `SENDING` row goes back to `PENDING` and is resent with the
same `event_id`.** One daemon owns this database, so a row still claimed after a
restart was claimed by a process that no longer exists — nobody is coming back
for it. The consumer deduplicates the second delivery. This is at-least-once,
deliberately, and it is the reason ADR 004's requirement on the server is not
optional.

**No leases, no distributed locks, no heartbeats, no worker-ownership
framework.** There is one writer. Machinery for coordinating writers that do not
exist would be cost with no benefit and a false suggestion that PI.Edge runs
more than one dispatcher.

A timeout is treated the same way, for the same reason: it is the honest "we
asked and we do not know", and the only safe answer is to ask again with the
same id.

## Decision — the effect is durable before the cursor moves

Inbound messages arrive in order and carry the cursor position *after* them. The
rule is one-directional:

> The effect commits first. The cursor follows. Never the reverse.

A crash in between re-delivers the message, which is harmless because every
inbound message is idempotent on something the core already had — a catalog
content hash, a chunk hash, a `message_id` recorded in `processed_commands`, or
a projection whose second application writes the same value. The forbidden order
is the other one: a cursor ahead of local state would skip a message forever.

Where the effect is a direct projection, the effect and the cursor share one
transaction, which is the strongest form of the rule. Where the effect is an
existing multi-step service call — the catalog import — the cursor is written in
the transaction immediately after, because there is exactly one importer in
PI.Edge and the sync engine does not become a second one.

The first message that cannot be applied **stops the cycle**. It is not skipped,
and nothing after it is applied: skipping one would advance the cursor past an
effect that never happened. An inbound batch that cannot be read leaves an
import `IMPORTING` and never partially `ACTIVE`; activation stays atomic
(ADR 013).

**Inbound kinds are a closed set.** A server that invents a message produces a
parse failure at the edge rather than something to quietly ignore.

## Decision — what travels out

| Fact | Published | Why |
| --- | --- | --- |
| `SALE_FINALIZED` | yes | since milestone 2 |
| `REFUND_ACCEPTED` | yes, at `ACCEPTED`, version 1 | the commercial return is irrevocable from here |
| `REFUND_COMPLETED` | yes, at `COMPLETED`, version 2 | the financial obligation is settled |
| `INVENTORY_MOVEMENT_RECORDED` | manual movements only | **amends ADR 028**, which anticipated this |
| `REGISTER_SESSION_CLOSED` | yes | carries the whole session, float included |
| `BUSINESS_DAY_CLOSED` | yes | since milestone 7 |

**A refund is published twice**, as two versions of one aggregate keyed on
`refund_id`. Acceptance is a fact on its own: by then the goods are physically
back, the refundable quantities are consumed and `REFUND_RETURN` is written, and
none of that is undone because a provider answered `UNKNOWN`. A refund can sit
in `FINANCIAL_PENDING` indefinitely, so a consumer that only heard
`REFUND_COMPLETED` would not know the merchandise had returned until the money
resolved — which may be days (M9's frozen lifecycle: `CREATED -> ACCEPTED ->
FINANCIAL_PENDING -> COMPLETED`).

`REFUND_FINANCIAL_PENDING` is **not** published: nothing needs it, and an event
with no consumer is an event that drifts.

Both are enqueued in the **same SQLite transaction** as the transition they
describe and its commercial consequences, so there is no state in which a refund
is accepted and the fact about it is missing, nor one in which it is completed
and unreported. `event_id` is stable per durable fact: `AcceptRefund` is
idempotent on its `request_id`, and the aggregate refuses a second acceptance
anyway.

This is also the first productive aggregate with two versions, so the ordering
rule above stops being theoretical: version 2 of a refund is never in flight
before version 1 is acknowledged, and a `FAILED` version 1 holds back its
version 2 while every other aggregate keeps flowing.

Sale and refund stock movements are **not** published: they are derivable from
`SALE_FINALIZED` and `REFUND_COMPLETED`, and publishing them as well would hand
the consumer the same depletion twice. A count somebody typed in has no other
source, so it travels. An inventory bootstrap is not published either — it came
from upstream in the first place.

A session is published when it closes, not when it opens: the closed event
carries the entire session state, and an open session is not yet a settled fact.

## Consequences

* `outbox_events` was rebuilt (migration 0010) to widen its status set, carrying
  every row, both indexes, the delete guard and — critically — `seq` across.
* `attempt_count` is now meaningful: it drives the backoff and tells an operator
  how long something has been stuck.
* A till that never reaches a server accumulates `PENDING` rows forever, by
  design. Disk is cheap; lost revenue is not.
