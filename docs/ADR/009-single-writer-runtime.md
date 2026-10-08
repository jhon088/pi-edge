# ADR 009 — One runtime per installation

Status: accepted · 2026-10-08

## Context

SQLite's locking serializes writes, but it does not give us a single *runtime
identity*. Two processes could each hold a consistent database view and still
make conflicting decisions about the same register: two notions of the active
transaction, two allocators of `register_sequence`, two outbox writers, two
answers to "is the business day open". Nothing in SQLite prevents that, and a
POS cannot tolerate it.

## Decision

**One OS-level exclusive file lock per installation, acquired before the
database is opened.**

* The lock lives at `<db-dir>/.pi-edge/daemon.lock` and is taken with
  `File::try_lock` (`flock` on unix, `LockFileEx` on Windows).
* It is acquired **first**, before the credential is read and before SQLite is
  touched. A process that does not own the installation never opens the files.
* The operating system releases the lock when the holder's descriptor closes —
  including `SIGKILL`, a panic or a power cut. There is no stale-lock cleanup
  to get wrong, and therefore no "is that pid still alive?" guesswork.
* The lock file's contents (pid, endpoint, start time) are **diagnostics only**,
  written for a human reading the directory. The pid is never consulted to
  decide whether the lock is free; the OS lock is the only authority.
* A second runtime exits cleanly with a dedicated exit code (`2`) and a message
  naming the owner, and does not touch the database.
* The IPC endpoint is derived from the canonical database path, so one database
  means one endpoint and one runtime. The endpoint name is not a secret and is
  not an authentication mechanism (ADR 010).

## Consequences

* Starting a second daemon is a clear, fast failure rather than corruption:
  `a_second_runtime_refuses_to_start_on_the_same_installation` asserts the exit
  code, the message and that the incumbent keeps serving.
* Two databases on one machine (e.g. a test fixture and a real till) each get
  their own lock, endpoint and credential, and can run simultaneously.
* Tooling cannot "just open the database" while the daemon runs. That is the
  point; diagnostics go through the protocol (`edge-cli`), not around it.
* A network filesystem would make `flock` unreliable. The local database is
  local by definition; if that ever changes, this ADR has to change with it.
