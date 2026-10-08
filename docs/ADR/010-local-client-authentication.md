# ADR 010 — Local client authentication

Status: accepted · 2026-10-08

## Context

Once the core is a daemon with an IPC endpoint, "any local process" is a threat
surface. A process that can connect can open and close the business day, add
items, commit cash payments, finalize sales and read the journal — fraud and
data corruption, not just mischief. Root access and physical access to the
terminal are out of scope; another app or tool on the same device is not,
especially on Android where process isolation between apps is the norm and
`localhost` is shared.

## Decision

**Two layers: OS isolation plus a local credential.**

1. **OS isolation** is the real boundary.
   * The runtime directory `<db-dir>/.pi-edge/` is `0700`.
   * The unix socket is `0600`.
   * On Windows the pipe should carry an ACL restricted to the current user.
     **This is not implemented yet** — it needs Win32 security descriptors and
     we cannot test it on this machine — so on Windows the credential is
     currently the only layer. Tracked as work for the Windows milestone.

2. **A local credential**, because OS isolation is weaker or differently shaped
   on other transports (Android sandboxes, a future TCP fallback), and because
   the daemon's trust boundary should be explicit rather than implied by a file
   mode.
   * 256 bits from the OS CSPRNG, hex encoded.
   * Stored at `<db-dir>/.pi-edge/daemon.token`, created with `O_EXCL` and mode
     `0600` on first run, never in SQLite.
   * Presented once per connection in the `HELLO` frame and compared in
     constant time. Authentication is per connection: an event stream is a
     second connection and authenticates independently.
   * A request before a successful handshake is refused with
     `NOT_AUTHENTICATED`; a bad credential gets `AUTHENTICATION_FAILED` and the
     connection is closed. Failures are deliberately indistinguishable: the
     client never learns whether a credential exists, how long it should be, or
     where it lives.
   * It never appears in a log, a trace, an error message or the diagnostic UI.
     `HelloV1` and `AuthToken` have hand-written `Debug` implementations that
     print `<redacted>`, and a test asserts the token is absent from the
     daemon's debug-level logs and from every error string.
   * Clients obtain it through `LocalCredentialProvider`, never from a command
     line argument. The desktop implementation reads the file; Android will
     implement its own (app-private storage or the Keystore) without
     `EdgeClient` changing.

The credential authenticates a **process**, not a cashier. Operator
authentication is a separate concern and is not addressed here.

## Consequences

* **No rotation, no expiry.** The credential is an installation-level secret
  that survives restarts. Rotation is manual and coarse: delete the file and
  restart the daemon. Clients re-read it on every connect, so they pick up the
  new value with no coordination — but any client that cached it in its own
  config would break, so no client is allowed to.
* If the file is lost, the daemon regenerates it on the next start; recovery is
  a restart.
* A process running as the POS user is trusted. That is the honest boundary of
  a file-permission scheme; raising it would mean peer credential checks
  (`SO_PEERCRED`) or code signing, which we deliberately deferred.
* Explicitly **not** in this phase: remote rotation, certificates, mTLS, PKI,
  per-operator tokens.
