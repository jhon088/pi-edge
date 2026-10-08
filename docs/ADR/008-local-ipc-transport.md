# ADR 008 — Local IPC transport

Status: accepted · 2026-10-08

## Context

With `edge-daemon` as the runtime owner (ADR 007), every UI and the CLI need a
local transport. The targets are macOS (development), Windows POS, Linux POS
and Android/iMin, and a Kotlin Android service is a likely future host. The
choice is expensive to reverse because it shapes the client API.

Options considered:

| | Unix socket + named pipe | localhost TCP | local HTTP |
| --- | --- | --- | --- |
| Isolation | filesystem permissions / pipe ACL; unreachable from the network | any local process can connect; no OS peer identity | same as TCP |
| Android | works (Android is Linux), and `LocalSocket` in Kotlin is the same primitive | **`127.0.0.1` is shared between apps** | same problem |
| Windows | named pipe | may trigger a firewall prompt | same as TCP |
| Cost | one dependency (`interprocess`) | none | HTTP server + async runtime |
| Events | bidirectional stream, push is natural | same | needs SSE or WebSocket |
| Debugging | `socat`/`nc -U` | `nc` | `curl` (best) |

## Decision

**Unix domain socket on macOS/Linux, named pipe on Windows, behind one
abstraction.** `interprocess 2.4` provides both under a single API; it is the
only crate in the workspace that names either primitive.

* `EdgeTransport` is the client-side contract: `connect`, `request`,
  `subscribe`, `reconnect`, `close`. `EdgeListener`/`ServerConnection` are its
  server-side mirror, so the daemon does not name a socket either.
* `DesktopLocalTransport` is the implementation. `AndroidServiceTransport` is
  explicitly future work: **we do not assume production Android will use
  `AF_UNIX`.** When we reach iMin we will weigh Binder / an Android service
  against a sandbox-local socket, measured on the device. The Edge Protocol
  stays byte-identical either way.
* Nothing above `edge-ipc` may import a socket type. The domain never sees
  JSON, sockets or pipes at all.

**Framing: JSON Lines.** One compact JSON object per `\n`-terminated line.
`serde_json` escapes newlines inside strings, so the delimiter is unambiguous;
frames are capped at 4 MiB; blank lines are tolerated so a human can poke the
socket by hand.

**No async runtime.** The core is synchronous, and a register serves a handful
of connections. The daemon uses a blocking accept loop on its own thread plus
one thread per connection. Adding Tokio would buy nothing and would force a
runtime into a core that must also embed in an Android service.

**Events** (`EDGE_STATE_CHANGED`) are delivered on a *separate* connection that
the client opens with `subscribe()`. One connection is therefore either a
request channel or an event stream, never both, which means exactly one thread
ever writes to a socket and no frame can interleave. The alternative —
multiplexing both on one connection — needs a reader thread plus a correlation
map in every client; this is simpler and the second connection is free.

HTTP was rejected: it would add a server stack and an async runtime to the POS
runtime for two calls a second, and it pushes the design toward the local
microservice shape we ruled out. TCP was rejected mainly because of Android:
localhost does not isolate apps.

## Consequences

* The endpoint is a filesystem path on unix, which is bounded (104 bytes on
  macOS). `RuntimePaths` fails loudly if the path would be too long instead of
  truncating.
* A killed daemon leaves a stale socket file. Safe to remove, because the
  runtime lock (ADR 009) is acquired first: if we hold the lock, any endpoint
  file belongs to a dead process.
* No request timeout yet. The daemon handles each request on its own thread and
  never blocks indefinitely on business work, so the realistic failure is
  process death, which closes the socket and surfaces immediately. A timeout
  needs per-platform socket options and is deferred.
* Debugging needs `socat` rather than `curl`. Acceptable: the CLI is the
  supported way in, and `edge-cli watch` shows the event stream.
