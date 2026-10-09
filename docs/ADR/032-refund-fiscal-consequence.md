# ADR 032 — The fiscal consequence of a refund

Status: accepted (milestone 9)

## Context

In Mexico a refund of an invoiced sale owes a credit note. A refund of a sale
that was never invoiced owes nothing. PI.Edge has to know which case it is
in — and it issues no CFDI, signs nothing, and talks to no PAC or tax
authority.

## Decision — the state, and only the state

A refund records **which fiscal consequence was chosen**, and that is all:

| `RefundFiscalStatus` | Meaning |
| --- | --- |
| `NONE` | the original was never invoiced; there is no credit note |
| `CREDIT_NOTE_PENDING` | invoiced, and the credit note is owed later |
| `CREDIT_NOTE_REQUESTED` | invoiced, and the credit note was asked for now |

`CREDIT_NOTE_REQUESTED` is a **durable intention**, not a result: it records
that the credit note was asked for at the moment of the refund. It does **not**
mean stamped, issued or completed — nothing in PI.Edge can know that, which is
why `CREDIT_NOTE_COMPLETED` and `CREDIT_NOTE_FAILED` do not exist yet.

`CREDIT_NOTE_COMPLETED` and `CREDIT_NOTE_FAILED` are deliberately absent:
nothing in PI.Edge can produce them, and a state with no operation is a promise
rather than a model (frozen in milestone 7). They arrive with the fiscal
backend that can reach them.

No XML, no stamping, no `account.move`, no SAT, no Odoo vocabulary.

## Decision — the choice is constrained by the original sale

```
original NOT_INVOICED  ->  NONE only
original INVOICED      ->  CREDIT_NOTE_PENDING or CREDIT_NOTE_REQUESTED
```

Both directions are refused with `REFUND_FISCAL_ACTION_INVALID`. Offering a
credit note for an invoice that does not exist would be inventing a fiscal
document; letting an invoiced sale refund with `NONE` would leave the books
wrong. The operator chooses *between the legal options*, never freely.

The original's fiscal state is **materialised on the refund**
(`original_fiscal_state`), so invoicing the sale afterwards cannot rewrite the
history of a decision that was correct when it was made. A database `CHECK`
enforces the pairing, so the row cannot exist in a contradictory state.

## Decision — fiscal metadata lives beside the sale, not on it

A sale can be finalized today and invoiced tomorrow. That is a **different
lifecycle** from the sale's, and a finalized sale is otherwise immutable, so
the fiscal state does not belong on `retail_transactions`:

```sql
CREATE TABLE transaction_fiscal_metadata (
    transaction_id PRIMARY KEY,
    fiscal_state   TEXT NOT NULL CHECK (IN ('NOT_INVOICED', 'INVOICED')),
    invoice_ref    TEXT,
    updated_at     TEXT NOT NULL,
    CHECK ((fiscal_state = 'INVOICED') = (invoice_ref IS NOT NULL))
)
```

A 1:1 projection, not a crate and not a bounded context: there is a real
lifecycle boundary and nothing more to model yet.

`invoice_ref` is a canonical, opaque reference assigned upstream. Not a PAC
payload, not a CFDI field, not an ERP document id.

### Two states, not three

"Missing row" and "not invoiced" must not be the same thing read two ways, so
**the row is written when the sale finalizes**, in the same transaction, always
as `NOT_INVOICED`. A finalized sale therefore always has a row, and a missing
row for a finalized sale is a bug rather than a third state.

That is why there is no `UNKNOWN`. Adding it would mean a value with no
operation, and it would describe every sale in the system today, blocking every
refund for want of information nobody can supply.

## Decision — PI.Edge consumes fiscal metadata; it never declares it

**PI.Edge is not the authority for saying a sale was invoiced.** Invoicing
happens upstream, and the fiscal state is metadata PI.Edge *reads* in order to
decide what a refund owes. So there is **no production operation** that writes
it:

* nothing in Edge Protocol V1 — no operation, no DTO;
* nothing on `EdgeClient` or `IpcEdgeClient`;
* no arm in the daemon's dispatch;
* no CLI command.

A till therefore meets exactly one fiscal state: `NOT_INVOICED`, written when
the sale finalized. Asking for a credit note on a wire-reachable sale is
refused, and a daemon test pins that there is no message that could change it.

`CREATE_REFUND` carries a fiscal **action**, never a fiscal **state**. A
contract test enumerates the payload's keys and asserts it has no
`fiscal_state`, `invoice_ref`, `authority` or amount field at all.

### How the invoiced case is tested

Behind the `testing` feature of `edge-application`, and only there:
`MarkTransactionInvoicedCommand` plus
`EdgeApplication::mark_transaction_invoiced`. Test crates enable the feature;
the daemon does not, so the production binary cannot reach it even by mistake.
Integration, crash-recovery and golden suites use it as a fixture to stand in
for the upstream system.

When sync exists, the metadata arrives from PI.COMMERCE Server and this fixture
is replaced by the real thing — not promoted to a POS capability.

## Consequences

* A refund can be commercially complete with its credit note still pending, and
  the two are visible separately.
* The fiscal backend of PI.COMMERCE has an explicit, durable, auditable queue
  to act on: refunds whose `fiscal_status` is `CREDIT_NOTE_PENDING`.
* When stamping exists, it adds states and an operation; it changes nothing
  about what is recorded here.
