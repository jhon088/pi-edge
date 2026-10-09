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
  fetchCatalogStatus,
  fetchConnection,
  fetchCoreInfo,
  fetchEdgeState,
  fetchTransactionPayments,
  lookupBarcode,
  onStateChanged,
} from "./api";
import type {
  BridgeError,
  CatalogStatus,
  ConnectionStatus,
  CoreInfo,
  EdgeState,
  ProductLookup,
  TransactionPayments,
} from "./types";

/** Fallback refresh while the core is unreachable, so the UI notices its return. */
const RECONNECT_POLL_MS = 1000;

interface Snapshot {
  connection: ConnectionStatus;
  info: CoreInfo | null;
  state: EdgeState | null;
  catalog: CatalogStatus | null;
  /** Payments of the active sale, or null when there is no sale to ask about. */
  payments: TransactionPayments | null;
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
        const [info, state, catalog] = await Promise.all([
          fetchCoreInfo(),
          fetchEdgeState(),
          fetchCatalogStatus(),
        ]);
        // Payments belong to a sale, so they are only asked for when one is
        // open. The core owns the answer; nothing is derived here.
        const active = state.active_transaction;
        const payments = active
          ? await fetchTransactionPayments(active.transaction_id)
          : null;
        setSnapshot({ connection, info, state, catalog, payments, error: null });
      } catch (failure) {
        // The core is unreachable or refused: report it, invent nothing. In
        // particular no stale financial state is kept — a payment total that
        // might be wrong is worse than no total at all.
        setSnapshot({
          connection: await fetchConnection(),
          info: null,
          state: null,
          catalog: null,
          payments: null,
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

  const { connection, info, state, catalog, payments, error } = snapshot;

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

        <Card title="Catalog">
          {catalog?.active ? (
            <>
              <Field
                label="Active version"
                value={String(catalog.active.snapshot.version)}
              />
              <Field
                label="Products"
                value={String(catalog.active.stored.products)}
              />
              <Field
                label="Price books"
                value={String(catalog.active.stored.price_books)}
              />
              <Field
                label="Prices"
                value={String(catalog.active.stored.price_entries)}
              />
              <Field
                label="Barcodes"
                value={String(catalog.active.stored.barcodes)}
              />
              <Field
                label="Promotions"
                value={String(catalog.active.stored.promotions)}
              />
              <Field
                label="Tax profiles"
                value={String(catalog.active.stored.tax_profiles)}
              />
              <Field
                label="Last activation"
                value={catalog.active.snapshot.activated_at ?? "—"}
              />
              <Field
                label="Content hash"
                value={shortHash(catalog.active.snapshot.content_hash)}
                mono
              />
            </>
          ) : (
            <p className="muted">
              No active catalog: the register cannot start a sale.
            </p>
          )}
          <Field
            label="Pending version"
            value={
              catalog?.pending.length
                ? catalog.pending
                    .map((view) => String(view.snapshot.version))
                    .join(", ")
                : "—"
            }
          />
          {catalog?.importing.length ? (
            <Field
              label="Importing"
              value={catalog.importing
                .map((view) => `v${view.snapshot.version}`)
                .join(", ")}
            />
          ) : null}
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

      <Card title="Barcode lookup">
        <BarcodeLookup enabled={connection.connected} />
      </Card>

      <Card title="Active transaction">
        {state?.active_transaction ? (
          <ActiveTransaction transaction={state.active_transaction} />
        ) : (
          <p className="muted">No active transaction.</p>
        )}
      </Card>

      <Card title="Payments">
        {!connection.connected ? (
          <p className="muted">
            Core disconnected — no payment state is shown. Nothing here is
            cached, so this screen can never display a stale balance.
          </p>
        ) : state?.active_transaction && payments ? (
          <Payments payments={payments} />
        ) : (
          <p className="muted">No active transaction to collect.</p>
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

function shortHash(hash: string | null): string {
  if (!hash) {
    return "—";
  }
  const [algorithm, digest] = hash.split(":");
  return digest ? `${algorithm}:${digest.slice(0, 12)}…` : hash;
}

/**
 * Diagnostic lookup. The core resolves the product and states the price; this
 * component only shows what came back.
 */
function BarcodeLookup({ enabled }: { enabled: boolean }) {
  const [barcode, setBarcode] = useState("");
  const [result, setResult] = useState<ProductLookup | null>(null);
  const [error, setError] = useState<BridgeError | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!barcode.trim()) {
      return;
    }
    setBusy(true);
    try {
      setResult(await lookupBarcode(barcode.trim()));
      setError(null);
    } catch (failure) {
      setResult(null);
      setError(describeError(failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <form className="lookup" onSubmit={submit}>
        <input
          type="text"
          value={barcode}
          placeholder="Scan or type a barcode"
          onChange={(event) => setBarcode(event.target.value)}
          disabled={!enabled}
        />
        <button type="submit" disabled={!enabled || busy}>
          {busy ? "Resolving…" : "Resolve"}
        </button>
      </form>
      {result ? (
        <div className="row">
          <Field label="Product" value={result.product.name} />
          <Field label="Reference" value={result.product.product_ref} mono />
          <Field
            label="Unit price"
            value={
              result.price
                ? `${result.price.unit_price.formatted} (${result.price.price_mode})`
                : (result.price_issue ?? "no price")
            }
          />
          <Field
            label="Price book"
            value={result.price?.price_book_code ?? "—"}
          />
          <Field
            label="Tax"
            value={`${result.tax_profile.code} (${
              result.tax_profile.rate_bps / 100
            }%)`}
          />
          <Field label="Snapshot" value={`v${result.snapshot_version}`} />
          <Field label="Priced at" value={result.effective_at} />
        </div>
      ) : null}
      {error ? (
        <p className="muted">
          <code>{error.code}</code> {error.message}
        </p>
      ) : null}
      <p className="muted">
        The core resolves the price book, the price and the tax. React computes
        nothing.
      </p>
    </>
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
            <th>Book</th>
            <th className="num">Discount</th>
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
              <td className="mono">{line.price_book_id ?? "—"}</td>
              <td className="num">{line.discount_amount.formatted}</td>
              <td className="num">{line.tax_amount.formatted}</td>
              <td className="num">{line.line_total.formatted}</td>
            </tr>
          ))}
          {transaction.lines.length === 0 ? (
            <tr>
              <td colSpan={9} className="muted">
                No lines yet.
              </td>
            </tr>
          ) : null}
        </tbody>
      </table>
      {transaction.applied_promotions.length > 0 ? (
        <table className="lines">
          <thead>
            <tr>
              <th>Promotion</th>
              <th>Name</th>
              <th>Target</th>
              <th>Rule</th>
              <th className="num">Prio</th>
              <th>Stacking</th>
              <th className="num">Discount</th>
              <th>Detail</th>
            </tr>
          </thead>
          <tbody>
            {transaction.applied_promotions.map((applied) => (
              <tr key={applied.adjustment_id}>
                <td className="mono">{applied.code}</td>
                <td>{applied.name}</td>
                <td className="mono">
                  {applied.line_number === null
                    ? applied.target
                    : `${applied.target}:${applied.line_number}`}
                </td>
                <td className="mono">{applied.reason}</td>
                <td className="num">{applied.priority}</td>
                <td>{applied.stackability}</td>
                <td className="num">{applied.amount.formatted}</td>
                <td className="muted">{applied.detail ?? "—"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        <p className="muted">No promotion applies to this basket.</p>
      )}
      <div className="row totals">
        <Field label="Subtotal" value={transaction.totals.subtotal.formatted} />
        <Field label="Discounts" value={transaction.totals.discount_total.formatted} />
        <Field label="Net" value={transaction.totals.net_total.formatted} />
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

/**
 * Read-only payment view.
 *
 * Every number comes from the core's settlement: this component chooses no
 * amount, decides no status and offers no action. Paying, reconciling and
 * reversing are exercised through the CLI in this milestone.
 */
function Payments({ payments }: { payments: TransactionPayments }) {
  const { settlement } = payments;
  return (
    <>
      <div className="row totals">
        <Field label="Amount due" value={settlement.amount_due.formatted} />
        <Field label="Applied" value={settlement.committed_total.formatted} />
        <Field label="Remaining" value={settlement.remaining_balance.formatted} />
        <Field label="Tendered" value={settlement.tendered_total.formatted} />
        <Field label="Change due" value={settlement.change_due.formatted} />
      </div>
      <div className="row">
        <Field label="Basket" value={settlement.basket_locked ? "LOCKED" : "EDITABLE"} />
        <Field
          label="Financial state"
          value={
            settlement.requires_reconciliation
              ? "NEEDS RECONCILIATION"
              : settlement.requires_resolution
                ? "UNRESOLVED"
                : "RESOLVED"
          }
        />
        <Field label="Can finalize" value={settlement.can_finalize ? "YES" : "NO"} />
      </div>

      {payments.payments.length === 0 ? (
        <p className="muted">Nothing tendered yet.</p>
      ) : (
        <table className="lines">
          <thead>
            <tr>
              <th>Tender</th>
              <th>Provider</th>
              <th>Status</th>
              <th className="num">Requested</th>
              <th className="num">Applied</th>
              <th className="num">Change</th>
              <th>Created</th>
              <th>Id</th>
            </tr>
          </thead>
          <tbody>
            {payments.payments.map((payment) => (
              <tr key={payment.payment_id}>
                <td>{payment.tender_type}</td>
                <td className="mono">{payment.provider_kind ?? "—"}</td>
                <td>{payment.status}</td>
                <td className="num">{payment.requested_amount.formatted}</td>
                <td className="num">{payment.committed_amount.formatted}</td>
                <td className="num">{payment.change_due.formatted}</td>
                <td className="muted">{payment.created_at}</td>
                <td className="mono">{payment.payment_id}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {payments.payments.some((payment) => payment.attempts.length > 0) ? (
        <table className="lines">
          <thead>
            <tr>
              <th className="num">#</th>
              <th>Attempt status</th>
              <th className="num">Requested</th>
              <th className="num">Authorized</th>
              <th>Provider ref</th>
              <th>Failure</th>
              <th>Open</th>
              <th>Updated</th>
            </tr>
          </thead>
          <tbody>
            {payments.payments.flatMap((payment) =>
              payment.attempts.map((attempt) => (
                <tr key={attempt.attempt_id}>
                  <td className="num">{attempt.attempt_number}</td>
                  <td>{attempt.status}</td>
                  <td className="num">{attempt.requested_amount.formatted}</td>
                  <td className="num">
                    {attempt.authorized_amount?.formatted ?? "—"}
                  </td>
                  {/* The provider's own reference. No card data ever reaches
                      the core, so there is nothing here to redact. */}
                  <td className="mono">{attempt.provider_reference ?? "—"}</td>
                  <td className="mono">{attempt.failure_code ?? "—"}</td>
                  <td>{attempt.unresolved ? "YES" : "no"}</td>
                  <td className="muted">{attempt.updated_at}</td>
                </tr>
              )),
            )}
          </tbody>
        </table>
      ) : null}

      <p className="muted">
        Every amount above was computed by the core and is displayed verbatim.
        This window issues no payment commands.
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
