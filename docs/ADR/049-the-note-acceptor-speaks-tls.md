# ADR 049 — The note acceptor speaks TLS, and PX Kiosk accepts the certificate it signed itself

Status: accepted (showroom — PX Kiosk). Request model amended by ADR 051:
the routes, the decimal amount, the per-call token and the reading of
`/status` described below were written before the vendor's specification was
available and are superseded there; the TLS decisions stand.
Scope: `apps/tauri-pos/src-tauri/src/cashgenic.rs` and nothing else.

## Context

The CashGenic note acceptor on the bench answers on `192.168.88.240:44333`.
The first version of this client wrote plain HTTP/1.1 by hand onto a
`TcpStream` — a deliberate choice at the time, and the right instinct: six
calls against a box on a local switch do not justify an HTTP stack.

It never worked on Windows. The device serves its API **over TLS**, with a
certificate it generated itself. The client was writing a plain request at a
TLS listener and reading back a handshake it could not parse, so cash was
simply never available and the screen said the acceptor was offline. The
diagnosis took longer than it should have because "offline" is also what an
unplugged device looks like.

## Decision

### TLS, through `rustls`

One dependency, in the host — not in the core, which still opens no sockets it
did not already own. `rustls` is pure Rust: it drags in no OpenSSL and no
SChannel, so the macOS development build and the Windows installer compile the
same way. `native-tls` would have given `danger_accept_invalid_certs` for free
and a system dependency on two platforms with it.

### The certificate is accepted without being verified

This is the decision worth writing down. `AcceptTheDeviceOnTheBench` verifies
nothing: not the name, not the chain, not the expiry.

It is a named type rather than a closure **so that it has to be read before it
is used again**, and its doc comment says plainly that on anything carrying a
customer's money or identity it would be a vulnerability.

What makes it acceptable here, specifically:

* the acceptor is a box on a showroom LAN whose certificate names nothing and
  is signed by nobody — there is no name to check against and no chain to walk;
* the payload is an amount of pesos. **No cardholder data passes through this
  module and none can**: the device takes banknotes, and what comes back is a
  number; and
* the alternative is a demo that cannot take a banknote.

Nothing else in this repository opens a TLS connection. If something does
later, it does not get this verifier.

### TLS only. There is no plain-HTTP path.

An earlier draft of this module probed both schemes and remembered which
answered. That is gone: the unit on the bench serves HTTPS on 44333, confirmed
by hand, and a fallback for a configuration nobody has is a second transport to
keep working for no device. One scheme, one code path.

A `401` is an answer and is returned, not retried as a transport problem.

### `/token` is an OAuth2 password grant, form-encoded

This is the second half of why cash never worked, and it survived the first
fix. The device wants:

```text
POST /token
Content-Type: application/x-www-form-urlencoded

grant_type=password&username=ApiUserOne&password=1234
```

The module sent `{"username":…,"password":…}` as JSON. The device refuses it,
and the refusal looks exactly like a device that is not there.

So the body carries its own content type — `Body::Form` against `Body::Json` —
rather than every call assuming JSON, and the form values are percent-encoded,
because the credential is typed in by whoever set the kiosk up and an operator
whose password contains `&` must not end up sending two fields.

The shape is pinned by two tests: one asserting the header, the field order and
the exact body, and one that runs the whole conversation against a **real TLS
listener** with a self-signed certificate checked in beside the test. The
second exists because the bug was never in the logic — it was in the transport,
and a unit test of request bytes would not have caught either half of it.

### What the device actually says is what is read

`/status?eventVersion=2`, which is the shape firmware 2.1.49 answers with.
`errorCondition` is read and `"None"` is treated as healthy. `/system`'s
`systemVersion` names the unit when no model field is sent, and a `bills` or
`coins` module reporting `connected: false` is surfaced — a unit with a
disconnected hopper answers every call and still cannot take money.

A refusal now carries the device's own body, trimmed. "The device answered
401" sends somebody to read a manual; "401: bad credentials" does not.

## Consequences

* Money still never travels through a float. Amounts leave as a decimal string
  built with integer arithmetic and come back parsed as text.
* The credentials stay in `preferences.json` in the clear beside the database.
  That remains the right trade for a demo machine reconfigured by hand on a
  stand, and the wrong one for anything else — unchanged by this ADR and still
  stated where it lives.
* A device on a *routed* network would now be reachable over TLS with no
  verification. Nothing in this build dials anything but the address an
  operator typed into the kiosk's own Admin screen.
* A unit configured **without** TLS is not supported and will read as
  unreachable. That is deliberate: the confirmed hardware serves HTTPS, and the
  error message carries the device's own words so the next person does not
  spend an afternoon on it either.
