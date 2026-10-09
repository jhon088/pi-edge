# ADR 023 — Safe drop approval, and reopening a session

Status: accepted (milestone 7)

## Safe drop needs a second person

Moving money out of a drawer into a safe is the one routine cash operation a
single person should not be able to complete alone. The product decision is
already made: a safe drop requires supervisor approval.

```
RequestSafeDrop ──> PENDING_APPROVAL ──> ApproveSafeDrop ──> APPROVED
                          │                                    │
                          └──> REJECTED                         └─ SAFE_DROP movement
```

**A pending drop does not touch expected cash.** The money is still in the
drawer until someone with authority says it left. The `SAFE_DROP` movement —
the only thing that reduces expected cash — is written at approval, in the same
database transaction, with the approval's id on it.

`requester != approver` in V1. A drop whose approver is its requester is
refused with `SAFE_DROP_SELF_APPROVAL`. A future policy may relax this for a
lone-operator store, and when it does it will be an explicit, named policy, not
an accident.

Approving twice is idempotent: the second call returns the approved drop and
writes nothing, guarded by `UNIQUE (session_id, movement_type, source_ref)` on
the movement (ADR 021).

### The domain does not know what a manager is

There is no `if role == "manager"` anywhere in the domain. The domain is handed
an identity that the application layer has already authorised, and records it:

```
Approval {
    approval_id, action_type, aggregate_ref,
    requested_by, requested_at,
    approved_by?, decided_at?, status, reason?
}
```

That is the whole model. It is enough to audit who asked and who agreed, and it
is deliberately not an RBAC system: there are no roles, no permissions and no
policy evaluation here. When real authorisation arrives it will decide *whether*
to call `ApproveSafeDrop`, and this record will keep saying who did.

## Reopening a closed session

A closed session sometimes has to be reopened — a miscount, an interrupted
close, a drawer that was not actually emptied. Two models:

1. **Flip the same session back to `OPEN`.** One continuous thread, but it
   overwrites `closed_at`, the declaration and the variance. The evidence of
   the first close is gone.
2. **Open a new session linked to the closed one.** *(chosen)*

A close is an accounting fact. Reopening does not un-happen it.

```
RegisterSession A   CLOSED, declared 1 040.00, expected 1 050.00, variance −10.00
                        │ reopened_from
                        ▼
RegisterSession B   OPEN, opening_float 1 040.00
```

The new session's `OPENING_FLOAT` is the **declared** amount of the previous
close, not the expected one: the declaration is what the operator counted, and
it is the best available statement of the money physically present. If the
variance was real, it stays visible as the previous session's variance rather
than being quietly absorbed into a new opening float.

`ReopenRegisterSession` records `reopened_from_session_id`, the reason, the
actor and the instant, and journals `SESSION_REOPENED`. The closed session is
never mutated.

## Consequences

* A register accumulates a chain of sessions, which is already allowed: a
  register is not tied to one operator or one session per day.
* "Reopen" is therefore a misnomer in storage terms and an accurate one in
  operational terms. The operation keeps the operational name; the data keeps
  the truth.
* A reopened session can be closed again, producing a second declaration and a
  second variance. Both are permanent.
