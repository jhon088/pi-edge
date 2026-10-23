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
  fetchCashPosition,
  fetchCatalogStatus,
  fetchHardwareStatus,
  fetchRecentSales,
  fetchSyncStatus,
  fetchInventoryPosition,
  openDrawer,
  printTestPage,
  scannerKey,
  fetchRefundEligibility,
  fetchTransactionRefunds,
  fetchConnection,
  fetchCoreInfo,
  fetchEdgeState,
  fetchTransactionPayments,
  lookupBarcode,
  onStateChanged,
} from "./api";
import type {
  BridgeError,
  CashPosition,
  CatalogStatus,
  ConnectionStatus,
  CoreInfo,
  EdgeState,
  HardwareStatus,
  RecentSales,
  SyncStatus,
  InventoryPosition,
  ProductLookup,
  Refund,
  RefundEligibility,
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
  /** The drawer of the open session, or null when no session is open. */
  cash: CashPosition | null;
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
        // The drawer belongs to a session, so it is only asked for when one is
        // open. Every figure in it is the core's.
        const cash = state.session ? await fetchCashPosition() : null;
        setSnapshot({ connection, info, state, catalog, payments, cash, error: null });
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
          cash: null,
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

  const { connection, info, state, catalog, payments, cash, error } = snapshot;

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

      <Card title="Cash drawer">
        {!connection.connected ? (
          <p className="muted">
            Core disconnected — no cash position is shown. Nothing here is
            cached, so this screen can never display a stale drawer.
          </p>
        ) : cash ? (
          <CashDrawer cash={cash} />
        ) : (
          <p className="muted">No open session, so no drawer to report.</p>
        )}
      </Card>

      <Card title="Inventory">
        <InventoryLedger enabled={connection.connected} />
      </Card>

      <Card title="Hardware">
        <HardwarePanel enabled={connection.connected} onChanged={refresh} />
      </Card>

      <Card title="Sales">
        <SalesHistory enabled={connection.connected} />
      </Card>

      <Card title="Sync">
        <SyncPanel enabled={connection.connected} />
      </Card>

      <Card title="Refunds">
        <Refunds enabled={connection.connected} />
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

/**
 * Hardware diagnostics, and the keyboard-wedge capture.
 *
 * The two buttons are safe by construction: they ask for an *intent* —
 * "print a test page", "open the drawer" — and carry no address, no bytes and
 * no driver name. The core decides what that means and audits it.
 *
 * The capture forwards keystrokes to the Rust host and nothing else: the rule
 * for what counts as a scan lives in `edge-hardware`, so this component holds
 * no buffer, no timeout and no barcode logic (ADR 036).
 */
function HardwarePanel({
  enabled,
  onChanged,
}: {
  enabled: boolean;
  onChanged: () => void;
}) {
  const [status, setStatus] = useState<HardwareStatus | null>(null);
  const [error, setError] = useState<BridgeError | null>(null);
  const [busy, setBusy] = useState(false);
  const [capturing, setCapturing] = useState(false);
  const [lastScan, setLastScan] = useState<string | null>(null);

  const read = useCallback(async () => {
    setBusy(true);
    try {
      setStatus(await fetchHardwareStatus());
      setError(null);
    } catch (failure) {
      setStatus(null);
      setError(describeError(failure));
    } finally {
      setBusy(false);
    }
  }, []);

  const act = async (action: () => Promise<HardwareStatus>) => {
    setBusy(true);
    try {
      setStatus(await action());
      setError(null);
    } catch (failure) {
      // A device that refused is reported, and nothing commercial moved.
      setError(describeError(failure));
      void read();
    } finally {
      setBusy(false);
      onChanged();
    }
  };

  // While capturing, every keystroke in the window goes to the host. The
  // scanner is a very fast typist, and the host's parser is what tells the
  // difference between it and a person.
  useEffect(() => {
    if (!capturing || !enabled) {
      return;
    }
    const onKeyDown = (event: KeyboardEvent) => {
      void scannerKey(event.key)
        .then((transaction) => {
          if (transaction) {
            setLastScan(
              `${transaction.lines.length} line(s) · ${transaction.totals.grand_total.formatted}`,
            );
            onChanged();
          }
        })
        .catch((failure) => setError(describeError(failure)));
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [capturing, enabled, onChanged]);

  return (
    <>
      <div className="lookup">
        <button type="button" onClick={() => void read()} disabled={!enabled || busy}>
          {busy ? "Working…" : "Probe devices"}
        </button>
        <button
          type="button"
          onClick={() => void act(printTestPage)}
          disabled={!enabled || busy}
        >
          Print test page
        </button>
        <button
          type="button"
          onClick={() => void act(openDrawer)}
          disabled={!enabled || busy}
        >
          Open drawer
        </button>
        <button
          type="button"
          onClick={() => setCapturing((on) => !on)}
          disabled={!enabled}
        >
          {capturing ? "Stop scanner capture" : "Start scanner capture"}
        </button>
      </div>

      {capturing ? (
        <p className="muted">
          Capturing keystrokes for the scanner. The host decides what a scan is;
          this window only forwards keys.
          {lastScan ? ` Last scan added: ${lastScan}.` : ""}
        </p>
      ) : null}

      {status ? (
        <table className="lines">
          <thead>
            <tr>
              <th>Device</th>
              <th>Driver</th>
              <th>State</th>
              <th>Last success</th>
              <th>Last error</th>
            </tr>
          </thead>
          <tbody>
            {[status.scanner, status.printer, status.drawer].map((device) => (
              <tr key={device.kind}>
                <td>{device.kind}</td>
                <td className="mono">{device.driver ?? "—"}</td>
                <td>{device.state}</td>
                <td className="muted">{device.last_success_at ?? "—"}</td>
                <td className="mono">
                  {device.last_error_code ?? "—"}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        <p className="muted">
          Not probed yet. A configured device is not an available one, so
          nothing is claimed until it answers.
        </p>
      )}
      {error ? (
        <p className="muted">
          <code>{error.code}</code> {error.message}
        </p>
      ) : null}
      <p className="muted">
        A device that fails changes nothing commercial: a sale stays paid, the
        drawer ledger stays right, and the failure is recorded.
      </p>
    </>
  );
}

/**
 * Read-only operational history.
 *
 * Recent sales with what each owes in paper. It observes and nothing else:
 * there is no print button here, because driving a receipt is the runtime's
 * job and this window is a diagnostic surface, not the POS (ADR 043).
 */
function SalesHistory({ enabled }: { enabled: boolean }) {
  const [history, setHistory] = useState<RecentSales | null>(null);
  const [error, setError] = useState<BridgeError | null>(null);

  const read = useCallback(async () => {
    try {
      setHistory(await fetchRecentSales(10));
      setError(null);
    } catch (failure) {
      setHistory(null);
      setError(describeError(failure));
    }
  }, []);

  useEffect(() => {
    if (enabled) {
      void read();
    }
  }, [enabled, read]);

  if (error) {
    return (
      <p className="muted">
        <code>{error.code}</code> {error.message}
      </p>
    );
  }
  if (!history || history.sales.length === 0) {
    return <p className="muted">No finalized sales yet.</p>;
  }
  return (
    <>
      <table className="lines">
        <thead>
          <tr>
            <th>#</th>
            <th>Closed</th>
            <th>Total</th>
            <th>Receipt</th>
          </tr>
        </thead>
        <tbody>
          {history.sales.map((sale) => (
            <tr key={sale.transaction_id}>
              <td>{sale.register_sequence}</td>
              <td className="muted">{sale.finalized_at}</td>
              <td>{sale.grand_total.formatted}</td>
              <td>{sale.receipt_status ?? "—"}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <p className="muted">
        {history.attention.pending + history.attention.failed + history.attention.unknown === 0
          ? "Every sale has its receipt."
          : `Needs attention — pending ${history.attention.pending}, failed ${history.attention.failed}, unknown ${history.attention.unknown}. A sale is closed whatever its receipt did.`}
      </p>
    </>
  );
}

/**
 * Read-only sync view.
 *
 * Shows how much is waiting, how much the server rejected for good, and how
 * far the inbound stream got. It offers no way to force a cycle: dispatching
 * is the runtime's job, and a screen that could push facts would be a screen
 * that decides when the business is reported.
 *
 * `No sync transport configured` is a normal, healthy state — the till sells
 * and the facts keep accumulating.
 */
function SyncPanel({ enabled }: { enabled: boolean }) {
  const [status, setStatus] = useState<SyncStatus | null>(null);
  const [error, setError] = useState<BridgeError | null>(null);
  const [busy, setBusy] = useState(false);

  const read = useCallback(async () => {
    setBusy(true);
    try {
      setStatus(await fetchSyncStatus());
      setError(null);
    } catch (failure) {
      setStatus(null);
      setError(describeError(failure));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    if (enabled) {
      void read();
    }
  }, [enabled, read]);

  return (
    <>
      <div className="lookup">
        <button type="button" onClick={() => void read()} disabled={!enabled || busy}>
          {busy ? "Working…" : "Refresh"}
        </button>
      </div>
      {status ? (
        <>
          <dl className="totals">
            <dt>Transport</dt>
            <dd className="mono">{status.driver}</dd>
            <dt>Waiting</dt>
            <dd>{status.pending + status.sending}</dd>
            <dt>Delivered</dt>
            <dd>{status.sent}</dd>
            <dt>Rejected</dt>
            <dd>{status.failed}</dd>
            <dt>Oldest waiting</dt>
            <dd className="muted">{status.oldest_pending_at ?? "—"}</dd>
            <dt>Inbound cursor</dt>
            <dd className="mono">{status.inbound_cursor ?? "—"}</dd>
          </dl>
          {status.failed_facts.length > 0 ? (
            <table className="lines">
              <thead>
                <tr>
                  <th>Fact</th>
                  <th>Aggregate</th>
                  <th>Attempts</th>
                  <th>Reason</th>
                </tr>
              </thead>
              <tbody>
                {status.failed_facts.map((fact) => (
                  <tr key={fact.event_id}>
                    <td>{fact.event_type}</td>
                    <td className="mono">{fact.aggregate_id}</td>
                    <td>{fact.attempt_count}</td>
                    <td className="mono">{fact.error_code ?? "—"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : null}
          <p className="muted">
            {status.driver === "unconfigured"
              ? "No sync transport configured. The register works offline indefinitely and keeps every fact."
              : "Facts are delivered at least once; the server deduplicates them."}
          </p>
        </>
      ) : null}
      {error ? (
        <p className="muted">
          <code>{error.code}</code> {error.message}
        </p>
      ) : null}
    </>
  );
}

/**
 * Read-only refund view.
 *
 * Driven by the original sale's id, because that is what a refund is about. It
 * shows what the core says is still refundable — quantities, amounts and the
 * per-tender ceilings — and the refunds already recorded. It computes none of
 * it, and it offers no way to create, accept, pay or reconcile a refund: those
 * are exercised through the CLI in this milestone.
 */
function Refunds({ enabled }: { enabled: boolean }) {
  const [transactionId, setTransactionId] = useState("");
  const [eligibility, setEligibility] = useState<RefundEligibility | null>(null);
  const [refunds, setRefunds] = useState<Refund[]>([]);
  const [error, setError] = useState<BridgeError | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    const id = transactionId.trim();
    if (!id) {
      return;
    }
    setBusy(true);
    try {
      const [view, history] = await Promise.all([
        fetchRefundEligibility(id),
        fetchTransactionRefunds(id),
      ]);
      setEligibility(view);
      setRefunds(history);
      setError(null);
    } catch (failure) {
      setEligibility(null);
      setRefunds([]);
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
          value={transactionId}
          placeholder="Original transaction id"
          onChange={(event) => setTransactionId(event.target.value)}
          disabled={!enabled}
        />
        <button type="submit" disabled={!enabled || busy}>
          {busy ? "Reading…" : "Read refunds"}
        </button>
      </form>

      {eligibility ? (
        <>
          <div className="row">
            <Field label="Sale" value={eligibility.original_status} />
            <Field
              label="Ticket"
              value={String(eligibility.register_sequence)}
            />
            <Field label="Sold for" value={eligibility.original_total.formatted} />
            <Field
              label="Still refundable"
              value={eligibility.refundable_total.formatted}
            />
            <Field label="Fiscal" value={eligibility.fiscal_state} />
            <Field label="Invoice" value={eligibility.invoice_ref ?? "—"} mono />
          </div>
          {eligibility.original_status !== "FINALIZED" ? (
            <p className="muted">
              Only a finalized sale can be refunded.
            </p>
          ) : null}
          {eligibility.refund_in_progress ? (
            <p className="banner" role="status">
              A refund of this sale is in progress ({eligibility.refund_in_progress}).
              A second one cannot be created until it finishes.
            </p>
          ) : null}

          <table className="lines">
            <thead>
              <tr>
                <th>Line</th>
                <th>Product</th>
                <th className="num">Sold</th>
                <th className="num">Refunded</th>
                <th className="num">Remaining</th>
                <th className="num">Refundable</th>
              </tr>
            </thead>
            <tbody>
              {eligibility.lines.map((line) => (
                <tr key={line.line_number}>
                  <td>{line.line_number}</td>
                  <td className="mono">{line.product_ref}</td>
                  <td className="num">{line.sold_quantity}</td>
                  <td className="num">{line.refunded_quantity}</td>
                  <td className="num">{line.remaining_quantity}</td>
                  <td className="num">{line.remaining_gross.formatted}</td>
                </tr>
              ))}
            </tbody>
          </table>

          <table className="lines">
            <thead>
              <tr>
                <th>Tender</th>
                <th className="num">Paid</th>
                <th className="num">Refundable</th>
              </tr>
            </thead>
            <tbody>
              {eligibility.tenders.map((tender) => (
                <tr key={tender.payment_id}>
                  <td>{tender.tender_type}</td>
                  <td className="num">{tender.committed.formatted}</td>
                  <td className="num">{tender.remaining.formatted}</td>
                </tr>
              ))}
            </tbody>
          </table>

          {refunds.length === 0 ? (
            <p className="muted">No refunds for this sale.</p>
          ) : (
            refunds.map((refund) => <RefundCard key={refund.refund_id} refund={refund} />)
          )}
        </>
      ) : null}
      {error ? (
        <p className="muted">
          <code>{error.code}</code> {error.message}
        </p>
      ) : null}
      <p className="muted">
        A refund gives back what the original sale actually charged: the core
        prices it from the sale&apos;s own materialised amounts, never from
        today&apos;s prices or promotions.
      </p>
    </>
  );
}

/** One refund: commercial, financial, inventory and fiscal state together. */
function RefundCard({ refund }: { refund: Refund }) {
  return (
    <div className="row">
      <Field label="Refund" value={refund.refund_id} mono />
      <Field label="Status" value={refund.status} />
      <Field label="Given back" value={refund.gross.formatted} />
      <Field label="Still owed" value={refund.remaining_to_refund.formatted} />
      <Field
        label="Stock returned"
        value={refund.inventory_returned ? "YES" : "NOT YET"}
      />
      <Field label="Fiscal" value={refund.fiscal_status} />
      <Field label="Reason" value={refund.reason_code} mono />
      {refund.requires_reconciliation ? (
        <Field label="Needs" value="RECONCILIATION" />
      ) : null}
      {refund.payments.map((payment) => (
        <Field
          key={payment.refund_payment_id}
          label={`${payment.tender_type} refund`}
          value={`${payment.refunded.formatted} of ${payment.requested.formatted} (${payment.status})`}
        />
      ))}
    </div>
  );
}

/** How many movements the panel asks for. A view policy, not a core rule. */
const INVENTORY_MOVEMENT_LIMIT = 10;

/**
 * Read-only inventory view.
 *
 * It is driven by a product reference because that is the grain of the ledger:
 * a position is `Σ(IN) − Σ(OUT)` for one product, folded by the core over the
 * movements of *this* installation. It is what this till knows, not a
 * store-wide count. This component adds nothing up, and offers no way to move
 * stock — bootstrap, manual in and manual out are exercised through the CLI in
 * this milestone.
 */
function InventoryLedger({ enabled }: { enabled: boolean }) {
  const [productRef, setProductRef] = useState("");
  const [position, setPosition] = useState<InventoryPosition | null>(null);
  const [error, setError] = useState<BridgeError | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!productRef.trim()) {
      return;
    }
    setBusy(true);
    try {
      setPosition(
        await fetchInventoryPosition(
          productRef.trim(),
          INVENTORY_MOVEMENT_LIMIT,
        ),
      );
      setError(null);
    } catch (failure) {
      setPosition(null);
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
          value={productRef}
          placeholder="Product reference"
          onChange={(event) => setProductRef(event.target.value)}
          disabled={!enabled}
        />
        <button type="submit" disabled={!enabled || busy}>
          {busy ? "Reading…" : "Read ledger"}
        </button>
      </form>

      {position ? (
        <>
          <div className="row">
            <Field label="Product" value={position.product_ref} mono />
            <Field label="Store" value={position.store_id} />
            <Field label="On hand" value={position.on_hand} />
            <Field label="Movements" value={String(position.movement_count)} />
            <Field
              label="Last movement"
              value={position.last_movement_at ?? "—"}
            />
          </div>
          {position.on_hand_thousandths < 0 ? (
            <p className="banner" role="status">
              On hand is negative: this till has sold more than its ledger
              knows it received. The figure is shown as it is, not clamped.
            </p>
          ) : null}
          {position.movements.length === 0 ? (
            <p className="muted">No movements for this product.</p>
          ) : (
            <table className="lines">
              <thead>
                <tr>
                  <th>Movement</th>
                  <th>Dir</th>
                  <th className="num">Quantity</th>
                  <th>Source</th>
                  <th>Reason</th>
                  <th>At</th>
                </tr>
              </thead>
              <tbody>
                {position.movements.map((movement) => (
                  <tr key={movement.movement_id}>
                    <td className="mono">{movement.movement_type}</td>
                    <td>{movement.direction}</td>
                    <td className="num">{movement.quantity}</td>
                    <td className="mono">{movement.source_ref}</td>
                    <td className="mono">{movement.reason_code ?? "—"}</td>
                    <td className="muted">{movement.occurred_at}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </>
      ) : null}
      {error ? (
        <p className="muted">
          <code>{error.code}</code> {error.message}
        </p>
      ) : null}
      <p className="muted">
        The ledger is append-only: a correction is a new movement, never an
        edit. On hand is derived from these movements and is stored nowhere —
        it is this installation&apos;s projection, not a store-wide count.
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
 * Read-only cash view.
 *
 * Every figure is the core's: this component derives nothing and offers no
 * action. Cash in, cash out, safe drops, closes and reopens are exercised
 * through the CLI in this milestone.
 *
 * This is an *operational* view, so it does show the expected cash. The
 * cashier's close path never receives that figure — the blind close is
 * enforced by a separate operation whose payload cannot carry an amount.
 */
function CashDrawer({ cash }: { cash: CashPosition }) {
  return (
    <>
      <div className="row totals">
        <Field label="Opening float" value={cash.opening_float.formatted} />
        <Field label="Cash sales" value={cash.cash_sales.formatted} />
        <Field label="Cash in" value={cash.cash_in.formatted} />
        <Field label="Cash out" value={cash.cash_out.formatted} />
        <Field label="Safe drops" value={cash.safe_drops.formatted} />
        <Field label="Refunds paid" value={cash.cash_refunds.formatted} />
        <Field label="Expected cash" value={cash.expected_cash.formatted} />
      </div>
      <div className="row">
        <Field label="Register" value={cash.register_id} />
        <Field label="Operator" value={cash.operator_id} />
        <Field label="Session" value={cash.session_status} />
        <Field label="Tendered (audit)" value={cash.tendered_total.formatted} />
        <Field label="Change given (audit)" value={cash.change_total.formatted} />
      </div>

      {cash.pending_safe_drops.length > 0 ? (
        <table className="lines">
          <thead>
            <tr>
              <th>Pending safe drop</th>
              <th className="num">Amount</th>
              <th>Reason</th>
              <th>Requested by</th>
              <th>Requested at</th>
            </tr>
          </thead>
          <tbody>
            {cash.pending_safe_drops.map((drop) => (
              <tr key={drop.safe_drop_id}>
                <td className="mono">{drop.safe_drop_id}</td>
                <td className="num">{drop.amount.formatted}</td>
                <td className="mono">{drop.reason_code}</td>
                <td>{drop.requested_by}</td>
                <td className="muted">{drop.requested_at}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}

      {cash.movements.length === 0 ? (
        <p className="muted">No movements yet.</p>
      ) : (
        <table className="lines">
          <thead>
            <tr>
              <th>Movement</th>
              <th>Dir</th>
              <th className="num">Amount</th>
              <th className="num">Tendered</th>
              <th className="num">Change</th>
              <th>Reason</th>
              <th>Operator</th>
              <th>At</th>
            </tr>
          </thead>
          <tbody>
            {cash.movements.map((movement) => (
              <tr key={movement.movement_id}>
                <td className="mono">{movement.movement_type}</td>
                <td>{movement.direction}</td>
                <td className="num">{movement.amount.formatted}</td>
                <td className="num">{movement.tendered?.formatted ?? "—"}</td>
                <td className="num">{movement.change?.formatted ?? "—"}</td>
                <td className="mono">{movement.reason_code ?? "—"}</td>
                <td>{movement.operator_id}</td>
                <td className="muted">{movement.created_at}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <p className="muted">
        The ledger is append-only: a correction is a new compensating movement,
        never an edit. Expected cash is derived from these movements and is not
        stored anywhere.
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
