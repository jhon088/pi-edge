# ADR 004 — At-least-once sync, with explicit idempotency

Status: accepted · 2026-10-08

## Context

The terminal sells offline and reconciles later. Networks fail after the request
leaves and before the response arrives, so "exactly once" delivery is not
available. Retries must not duplicate sales, and a crash must not produce a
finalized sale that the server never hears about.

## Decision

**Transactional outbox.** Finalizing a sale writes, in the *same* SQLite
transaction: the `retail_transactions` row (+ lines, payments), the
`journal_events` row, and the `outbox_events` row. There is no window in which
one exists without the others. The same applies to session close and business
day close.

**At-least-once, deduplicated by the consumer.** `outbox_events.event_id` is a
UUID and is the server-side idempotency key; `aggregate_version` gives ordering.
Status is `PENDING -> SENDING -> SENT`. Rows that are not `SENT` can never be
deleted (enforced by trigger): undelivered revenue is never dropped silently.
`attempt_count` / `next_attempt_at` exist for the retry policy.

**Explicit command idempotency.** Every mutating command accepts an optional
`request_id`. The first execution records its serialized result in
`processed_commands` inside the same transaction; a replay returns that result
instead of applying the command again. This is what makes a client retry (lost
IPC reply, double-tap, restarted UI) safe.

**Per-register sequence.** `UNIQUE(register_id, register_sequence)` gives the
server a gap-detectable, human-auditable ordering per terminal.

**Not built yet, deliberately:** the HTTP client and the dispatcher loop. No
`edge-sync` crate exists because it would have no behaviour to own in this
milestone; the durable contract it will consume (the outbox table and
`OutboxEvent`) is already fixed.

> **Amended by ADR 040 and ADR 041 (milestone 11).** The dispatcher and the
> `edge-sync` crate now exist, and the status set gained a terminal `FAILED`.
> The HTTP client still does not: the only transport is a simulator, chosen
> explicitly. Two things this ADR left open are now decided — a `SENDING` row
> found at startup goes back to `PENDING` and is resent with the same
> `event_id` (so the consumer's idempotency requirement is load-bearing, not
> theoretical), and retention stays deferred: nothing is purged.

## Consequences

* The server must be idempotent on `event_id`. This is a hard requirement on
  PI.COMMERCE Server, not a suggestion.
* The outbox grows. Reconciliation/retention is a future decision; keeping
  `SENT` rows is currently cheap and useful for audits. Still deferred as of
  milestone 11 (ADR 041).
* Recovery is simple: on start, pending rows are just still pending. Nothing
  needs to be replayed from memory, because nothing important ever lived there.
