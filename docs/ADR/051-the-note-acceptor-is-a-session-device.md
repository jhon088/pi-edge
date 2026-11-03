# ADR 051 — The note acceptor is a session device, with one link and one reader of its events

Status: accepted (showroom — PX Kiosk)
Amends ADR 049. Keeps its transport decisions (TLS through `rustls`, the
self-signed certificate accepted by `AcceptTheDeviceOnTheBench`) and replaces
its request model.

## Context

ADR 049 was written before PI.Edge had the vendor's document. With
*CashGenic API Specification v1* (Innovative Technology) in hand, the client it
describes turned out to be modelled as a stateless REST service, which the
device is not:

* Session commands went to routes that do not exist (`POST /PayAmount`,
  `/CancelPayment`, `/CloseSession`) with the amount as a decimal string. The
  document has one route, `POST /session`, with
  `{"request":"PayAmount","value":12345}` — an integer of minor units.
* `responseCode` was never read, so any HTTP 200 counted as accepted.
* `/status` was read for fields it does not have (a root `state`,
  `deposited`, `cashTotal`). It "returns a list of events since the last
  Status request" (p. 45), and three components read it independently: the
  health watcher, the payment screen's poll and "Probar conexión". Each read
  consumed events the others then never saw.
* Every round asked for a new token and for `/system`, which is system
  information, not a heartbeat: eighteen TLS connections a minute while idle, and
  about ninety during a thirty-second payment.
* Nothing serialised requests, the network ran on the window's main thread,
  and a cancelled session was never closed — so the next `PayAmount` would meet
  `REQUEST_FAIL_SESSION_ACTIVE`.
* Every response was read to the end of the stream, ignoring
  `Content-Length`, so a device that leaves the connection open costs a full
  read budget per call.

The tests passed throughout, because the test server answered the protocol the
client had invented.

## Decision

**One runtime per process** (`cashgenic::runtime`):

* **One link.** Every request — token, system, session, status — goes through
  one mutex, so the device never sees two at once. It is held for the exchange
  and its bookkeeping, never while a screen waits for a person. The network
  runs off the main thread (`spawn_blocking`); the snapshot is a separate lock
  and never waits on the network.
* **One token**, kept for subsequent requests. A `401` drops it, a new one is
  requested and *that* request is sent once more — once. `expires_in` is not
  interpreted: the document calls it "UTC in milliseconds since epoch" and its
  own example is `43200000`. A transport failure is never retried.
* **One reader of `/status?eventVersion=2`**: the authority thread, every
  10 s with no session and every 800 ms with one, woken at once by a
  `PayAmount`. `cashgenic_status`, `/demo/status`, the alerts and Admin read
  the state it keeps. "Probar conexión" is a grant if needed and `GET /system`
  through the same link — never `/status`.
* **`/system` is read once per device**, at start and again when the address
  or credentials change, and on "Probar conexión". Connection state comes from
  `paymentDevices[].isConnected`.
* **The session is followed from the events and closed by the authority**,
  only after a terminal event of this kiosk's transaction: `PaymentComplete`,
  `CancelComplete` (or `RefundComplete` after a cancel), `StopComplete`.
  `StopCancelled` returns to paying. `CloseSession` answered `0` or `13`
  closes it. A running session is never closed blind.
* **`10`/`14` on `PayAmount`** are recovered only when the open session is
  this kiosk's own and an event already finished it: `CloseSession`, then
  `PayAmount` once more. Otherwise the session is reported and left alone.
* **The payment screen completes on `COMPLETE`**, which is `PaymentComplete`
  — never on "outstanding is zero".
* **Health** is derived, not a last HTTP result: `DISABLED`, `READY`,
  `SESSION_ACTIVE`, `OUT_OF_SERVICE` (out-of-service events, an
  `errorCondition`, a disconnected payment device, code `11`) and
  `COMMUNICATION_ERROR` (three consecutive failed exchanges). A response code
  is an answer and never counts against communication. `/demo/status` keeps
  its fields: `online` is `READY` or `SESSION_ACTIVE`, `state` is still the
  device's last event or `OFFLINE`/`DISABLED`/`UNKNOWN`, `cash_total` is
  `cashStatus.totalSystemCash`.
* **HTTP framing**: chunked is decoded, `Content-Length` is read exactly and
  ends the response, and the end of the stream is the delimiter only when
  neither is present. 256 KiB at most.
* **Three budgets**: connect, TLS handshake and response, separately. They are
  deliberately generous (10 s, 15 s, 15 s) because the stand has shown
  successful handshakes of 6–7 s; they are tightened with measurements of
  this client, not before.

## Consequences

* Idle: 6 requests and 6 connections a minute, against 18 before. A payment:
  the `PayAmount`, one `/status` per 800 ms, the `CloseSession` — no token and
  no `/system`.
* `StopPayment`, refunds, refills, collections and age verification remain out
  of scope: nothing in PX Kiosk asks for them. Their events are still logged,
  and a stop started at the device is followed and closed.
* `InsufficientFloat`, `PayoutFailed`, `RestoreTransaction` and `PayOutError`
  are put in the acceptor's `detail` as needing a manual payout. No new
  alert or field was added for them.
* The test bench now answers only what the document defines, so a return to
  the invented routes, a decimal amount, root status fields or an ignored
  `responseCode` fails the tests.
* Validated against the document and the bench, not yet against the unit on
  the stand. What the document leaves open — whether the event queue is per
  client or global, what follows `StopCancelled` — is logged
  (`CASHGENIC_EVENT`) rather than assumed.
