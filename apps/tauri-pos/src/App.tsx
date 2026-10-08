/**
 * PI.Edge Diagnostic.
 *
 * Shows what the core reports and nothing else. It holds no business state: a
 * push notification from the core triggers a fresh query rather than a local
 * patch, so the screen can never drift from the runtime.
 */
import { useCallback, useEffect, useRef, useState } from "react";

import {
  describeError,
  fetchConnection,
  fetchCoreInfo,
  fetchEdgeState,
  onStateChanged,
} from "./api";
import type {
  BridgeError,
  ConnectionStatus,
  CoreInfo,
  EdgeState,
} from "./types";

/** Fallback refresh while the core is unreachable, so the UI notices its return. */
const RECONNECT_POLL_MS = 1000;

interface Snapshot {
  connection: ConnectionStatus;
  info: CoreInfo | null;
  state: EdgeState | null;
  error: BridgeError | null;
}

export default function App() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [lastEvent, setLastEvent] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const inFlight = useRef(false);

  const refresh = useCallback(async () => {
    if (inFlight.current) {
      return;
    }
    inFlight.current = true;
    setRefreshing(true);
    try {
      const connection = await fetchConnection();
      try {
        const [info, state] = await Promise.all([
          fetchCoreInfo(),
          fetchEdgeState(),
        ]);
        setSnapshot({ connection, info, state, error: null });
      } catch (failure) {
        // The core is unreachable or refused: report it, invent nothing.
        setSnapshot({
          connection: await fetchConnection(),
          info: null,
          state: null,
          error: describeError(failure),
        });
      }
    } finally {
      inFlight.current = false;
      setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    let unlisten: (() => void) | undefined;
    void onStateChanged((event) => {
      setLastEvent(`${event.reason} (revision ${event.revision})`);
      void refresh();
    }).then((stop) => {
      unlisten = stop;
    });
    return () => unlisten?.();
  }, [refresh]);

  // While disconnected there are no push events to wake us, so poll slowly
  // until the core is back. Connected, the event stream does the work.
  const connected = snapshot?.connection.connected ?? false;
  useEffect(() => {
    if (connected) {
      return;
    }
    const timer = setInterval(() => void refresh(), RECONNECT_POLL_MS);
    return () => clearInterval(timer);
  }, [connected, refresh]);

  if (!snapshot) {
    return (
      <main className="app">
        <h1>PI.Edge Diagnostic</h1>
        <p className="muted">Contacting the core…</p>
      </main>
    );
  }

  const { connection, info, state, error } = snapshot;

  return (
    <main className="app">
      <header className="header">
        <div>
          <h1>PI.Edge Diagnostic</h1>
          <p className="muted">
            Read-only view of the runtime. This window owns no POS state.
          </p>
        </div>
        <button type="button" onClick={() => void refresh()} disabled={refreshing}>
          {refreshing ? "Refreshing…" : "Refresh"}
        </button>
      </header>

      <ConnectionBanner connection={connection} error={error} />

      <section className="grid">
        <Card title="Core">
          <Field label="IPC connection" value={connection.connected ? "CONNECTED" : "DISCONNECTED"} />
          <Field label="Endpoint" value={connection.endpoint} mono />
          <Field label="Transport" value={connection.mode} />
          <Field label="Core status" value={info?.status ?? "—"} />
          <Field label="Core version" value={info?.core_version ?? "—"} />
          <Field label="Protocol version" value={info ? String(info.protocol_version) : "—"} />
          <Field
            label="DB schema version"
            value={info ? String(info.database_schema_version) : "—"}
          />
          <Field label="Uptime" value={info ? `${info.uptime_seconds}s` : "—"} />
          <Field label="Revision" value={info ? String(info.revision) : "—"} />
          <Field label="Runtime pid" value={info ? String(info.pid) : "—"} />
        </Card>

        <Card title="Business day">
          {state?.business_day ? (
            <>
              <Field label="Status" value={state.business_day.status} />
              <Field label="Id" value={state.business_day.business_day_id} mono />
              <Field label="Store" value={state.business_day.store_id} />
              <Field label="Opened at" value={state.business_day.opened_at} />
            </>
          ) : (
            <p className="muted">No open business day.</p>
          )}
        </Card>

        <Card title="Register session">
          {state?.session ? (
            <>
              <Field label="Status" value={state.session.status} />
              <Field label="Register" value={state.session.register_id} />
              <Field label="Operator" value={state.session.operator_id} />
              <Field label="Opening cash" value={state.session.opening_cash.formatted} />
              <Field label="Id" value={state.session.session_id} mono />
            </>
          ) : (
            <p className="muted">No open session.</p>
          )}
        </Card>

        <Card title="Outbox">
          <Field
            label="Pending events"
            value={state ? String(state.pending_outbox) : "—"}
          />
          <p className="muted">
            Undelivered work is never dropped. Sync is not implemented yet.
          </p>
        </Card>
      </section>

      <Card title="Active transaction">
        {state?.active_transaction ? (
          <ActiveTransaction transaction={state.active_transaction} />
        ) : (
          <p className="muted">No active transaction.</p>
        )}
      </Card>

      <footer className="muted footer">
        <span>
          Commands {connection.can_issue_commands ? "enabled" : "disabled"} ·
          {" "}
          {lastEvent ? `last event: ${lastEvent}` : "no events yet"}
        </span>
      </footer>
    </main>
  );
}

function ConnectionBanner({
  connection,
  error,
}: {
  connection: ConnectionStatus;
  error: BridgeError | null;
}) {
  if (connection.connected && !error) {
    return null;
  }
  const retry =
    connection.reconnect_in_ms !== null
      ? ` Retrying in ${Math.ceil(connection.reconnect_in_ms / 100) / 10}s.`
      : "";
  return (
    <div className="banner" role="status">
      <strong>Core disconnected.</strong> The runtime is not answering, so this
      screen shows no state and no command can be issued.{retry}
      {error ? <code className="banner-code">{error.code}</code> : null}
    </div>
  );
}

function ActiveTransaction({
  transaction,
}: {
  transaction: NonNullable<EdgeState["active_transaction"]>;
}) {
  return (
    <>
      <div className="row">
        <Field label="Status" value={transaction.status} />
        <Field label="Sequence" value={String(transaction.register_sequence)} />
        <Field label="Id" value={transaction.transaction_id} mono />
      </div>
      <table className="lines">
        <thead>
          <tr>
            <th>#</th>
            <th>Product</th>
            <th>Description</th>
            <th className="num">Qty</th>
            <th className="num">Unit</th>
            <th className="num">Tax</th>
            <th className="num">Total</th>
          </tr>
        </thead>
        <tbody>
          {transaction.lines.map((line) => (
            <tr key={line.line_id}>
              <td>{line.line_number}</td>
              <td className="mono">{line.product_ref}</td>
              <td>{line.description}</td>
              <td className="num">{line.quantity}</td>
              <td className="num">{line.unit_price.formatted}</td>
              <td className="num">{line.tax_amount.formatted}</td>
              <td className="num">{line.line_total.formatted}</td>
            </tr>
          ))}
          {transaction.lines.length === 0 ? (
            <tr>
              <td colSpan={7} className="muted">
                No lines yet.
              </td>
            </tr>
          ) : null}
        </tbody>
      </table>
      <div className="row totals">
        <Field label="Subtotal" value={transaction.totals.subtotal.formatted} />
        <Field label="Tax" value={transaction.totals.tax_total.formatted} />
        <Field label="Grand total" value={transaction.totals.grand_total.formatted} />
        <Field label="Paid" value={transaction.paid_total.formatted} />
        <Field label="Balance due" value={transaction.balance_due.formatted} />
        <Field label="Change due" value={transaction.change_due.formatted} />
      </div>
      <p className="muted">
        Every amount above was computed by the core and is displayed verbatim.
      </p>
    </>
  );
}

function Card({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="card">
      <h2>{title}</h2>
      {children}
    </section>
  );
}

function Field({
  label,
  value,
  mono = false,
}: {
  label: string;
  value: string;
  mono?: boolean;
}) {
  return (
    <div className="field">
      <span className="label">{label}</span>
      <span className={mono ? "value mono" : "value"}>{value}</span>
    </div>
  );
}
