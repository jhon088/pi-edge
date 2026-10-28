/**
 * PX Kiosk — the retail self-checkout lane.
 *
 * ```text
 * attract → lane ⇄ search → method → payment → done ┐
 *    ↑                                              │
 *    └────── idle timeout, or the ticket ends ───────┘
 * ```
 *
 * The same core, the same catalog and the same payment journey as the
 * restaurant: what changes is the channel this shell reads the menu for and
 * the fact that the lane **opens on the first scan** rather than on a button
 * (ADR 044). The sale lives in the core, so navigating cannot lose it and a
 * reload picks it back up.
 */
import { useCallback, useEffect, useRef, useState } from "react";

import { AdminScreen } from "./admin";
import {
  activeTransaction,
  cancelTransaction,
  changeQuantity,
  describe,
  fetchMenu,
  fetchPrinterSettings,
  removeItem,
  retailAddItem,
  scanBarcode,
  scanKey,
  totalize,
} from "./api";
import { CameraCorner } from "./CameraCorner";
import {
  Brand,
  ErrorBoundary,
  Header,
  Screen,
  Splash,
  Toast,
} from "./components";
import { usePaymentJourney } from "./payment";
import {
  RetailAttractScreen,
  RetailCartScreen,
  RetailSearchScreen,
  RetailTotalPanel,
} from "./retail";
import {
  CashScreen,
  ConfirmationScreen,
  MethodScreen,
  PaymentScreen,
  type Method,
} from "./screens";
import type { Menu, MenuItem, RetailTransaction } from "./types";
import "./kiosk.css";

/** How long a lane may sit untouched before the basket is abandoned. */
const IDLE_MS = 90_000;

/** How long the ticket stays up before the lane frees itself. */
const CONFIRM_SECONDS = 4 satisfies 4;

type Stage =
  | "loading"
  | "attract"
  | "lane"
  | "search"
  | "admin"
  | "method"
  | "payment"
  | "done";

export default function RetailApp({
  channel,
  cameraPreview,
  onLeave,
}: {
  /** The channel this lane sells from. Chosen by the host, not here. */
  channel: string;
  /** Whether this box shows the customer their own camera. */
  cameraPreview: boolean;
  /** Called when an operator switched the installation to another vertical. */
  onLeave: () => void;
}) {
  const [generation, setGeneration] = useState(0);
  return (
    <ErrorBoundary
      key={generation}
      onReset={() => setGeneration((value) => value + 1)}
    >
      <Retail
        channel={channel}
        cameraPreview={cameraPreview}
        onLeave={onLeave}
      />
    </ErrorBoundary>
  );
}

function Retail({
  channel,
  cameraPreview,
  onLeave,
}: {
  channel: string;
  cameraPreview: boolean;
  onLeave: () => void;
}) {
  const [stage, setStage] = useState<Stage>("loading");
  const [menu, setMenu] = useState<Menu | null>(null);
  const [transaction, setTransaction] = useState<RetailTransaction | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [method, setMethod] = useState<Method>("CARD");
  const [autoPrint, setAutoPrint] = useState(false);
  const [adminFrom, setAdminFrom] = useState<Stage>("attract");
  const [confirmed, setConfirmed] = useState<{
    order: string;
    total: string;
    method: Method;
  } | null>(null);
  const [countdown, setCountdown] = useState(CONFIRM_SECONDS);

  const complain = useCallback((value: unknown) => {
    setError(describe(value));
  }, []);

  useEffect(() => {
    if (!error) {
      return;
    }
    const timer = window.setTimeout(() => setError(null), 4200);
    return () => window.clearTimeout(timer);
  }, [error]);

  // --- the shelf, and whatever was already on the belt -------------------
  //
  // A basket that is open in the core is the customer standing there right
  // now, so a reload goes back to the lane rather than to the attract screen.
  // The core is the one that knows there is one; this only asks.
  useEffect(() => {
    let live = true;
    void Promise.all([fetchMenu(channel), activeTransaction().catch(() => null)])
      .then(([shelf, open]) => {
        if (!live) {
          return;
        }
        setMenu(shelf);
        setTransaction(open);
        setStage(open && open.lines.length > 0 ? "lane" : "attract");
      })
      .catch((reason) => {
        if (live) {
          complain(reason);
          setStage("attract");
        }
      });
    return () => {
      live = false;
    };
  }, [channel, complain]);

  // --- does this lane print its own tickets? -----------------------------
  const refreshAutoPrint = useCallback(() => {
    void fetchPrinterSettings()
      .then((config) =>
        setAutoPrint(config.settings.enabled && config.settings.auto_print),
      )
      .catch(() => setAutoPrint(false));
  }, []);

  useEffect(refreshAutoPrint, [refreshAutoPrint]);

  // --- payment: the restaurant's journey, unchanged ----------------------
  const { paying, reset: resetPaying } = usePaymentJourney({
    active: stage === "payment",
    transaction,
    method,
    autoPrint,
    onApproved: (finalized) => {
      setConfirmed({
        order: finalized.order_code,
        total: `$${finalized.totals.grand_total.formatted}`,
        method,
      });
      setTransaction(null);
      setStage("done");
    },
    onRefused: (reason) => {
      complain(reason);
      setStage("lane");
    },
  });

  // --- free the lane rather than leave a basket on it --------------------
  const clear = useCallback(
    async (cancel: boolean) => {
      if (cancel) {
        try {
          await cancelTransaction("LANE_ABANDONED");
        } catch {
          // Nothing open, or it already closed. The core is the one that
          // knows, and it just said so.
        }
      }
      setTransaction(null);
      setConfirmed(null);
      resetPaying();
      setMethod("CARD");
      setStage("attract");
    },
    [resetPaying],
  );

  useEffect(() => {
    if (stage === "attract" || stage === "loading" || stage === "done") {
      return;
    }
    let timer = 0;
    const arm = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        void clear(true);
      }, IDLE_MS);
    };
    arm();
    const events: (keyof WindowEventMap)[] = [
      "pointerdown",
      "keydown",
      "wheel",
    ];
    for (const event of events) {
      window.addEventListener(event, arm, { passive: true });
    }
    return () => {
      window.clearTimeout(timer);
      for (const event of events) {
        window.removeEventListener(event, arm);
      }
    };
  }, [stage, clear]);

  // --- the ticket counts itself down -------------------------------------
  useEffect(() => {
    if (stage !== "done") {
      return;
    }
    setCountdown(CONFIRM_SECONDS);
    const timer = window.setInterval(() => {
      setCountdown((value) => {
        if (value <= 1) {
          window.clearInterval(timer);
          void clear(false);
          return 0;
        }
        return value - 1;
      });
    }, 1000);
    return () => window.clearInterval(timer);
  }, [stage, clear]);

  // --- the reader -------------------------------------------------------
  //
  // A keyboard-wedge scanner types into whatever window has focus, so every
  // keystroke is forwarded to the parser in Rust and *that* decides when a
  // barcode is complete. React never assembles one, never resolves a product
  // and never prices anything (ADR 036).
  //
  // Only the three shopping screens listen. A reader swiped over the glass
  // while the customer is paying must not quietly add a bar of chocolate to a
  // sale that is already being authorised.
  const scanning = stage === "attract" || stage === "lane" || stage === "search";
  const scanningRef = useRef(scanning);
  scanningRef.current = scanning;

  useEffect(() => {
    if (!scanning) {
      return;
    }
    const onKey = (event: KeyboardEvent) => {
      // Modified keys are shortcuts, not barcodes.
      if (event.ctrlKey || event.metaKey || event.altKey) {
        return;
      }
      void scanKey(event.key)
        .then((updated) => {
          if (!updated || !scanningRef.current) {
            return;
          }
          setTransaction(updated);
          setStage("lane");
        })
        .catch(complain);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [scanning, complain]);

  // --- actions ------------------------------------------------------------

  const mutate = async (work: () => Promise<RetailTransaction>) => {
    setBusy(true);
    try {
      setTransaction(await work());
    } catch (reason) {
      complain(reason);
    } finally {
      setBusy(false);
    }
  };

  /** A product found on screen. The host opens the lane if it is not open. */
  const take = async (item: MenuItem) => {
    setBusy(true);
    try {
      setTransaction(await retailAddItem(item.product_ref, 1));
      setStage("lane");
    } catch (reason) {
      complain(reason);
    } finally {
      setBusy(false);
    }
  };

  /** A barcode typed in. The same path a reader takes, to the character. */
  const typed = async (barcode: string) => {
    setBusy(true);
    try {
      setTransaction(await scanBarcode(barcode));
      setStage("lane");
    } catch (reason) {
      complain(reason);
    } finally {
      setBusy(false);
    }
  };

  const checkout = async () => {
    setBusy(true);
    try {
      setTransaction(await totalize());
      resetPaying();
      setStage("method");
    } catch (reason) {
      complain(reason);
    } finally {
      setBusy(false);
    }
  };

  const browse = () => {
    // The shelf is browsable before anything is scanned: the first thing
    // added opens the lane, whichever way it is added.
    setStage("search");
  };

  const openAdmin = (from: Stage) => {
    setAdminFrom(from);
    setStage("admin");
  };

  // --- render -------------------------------------------------------------

  const units = (transaction?.lines ?? []).reduce(
    (sum, line) => sum + Math.max(1, Math.round(line.quantity_thousandths / 1000)),
    0,
  );

  if (stage === "admin") {
    return (
      <div className="k-app">
        <AdminScreen
          saleOpen={transaction !== null}
          onReset={() => {
            // The core already abandoned the basket; clear what the screen
            // was holding and free the lane.
            setTransaction(null);
            setConfirmed(null);
            resetPaying();
            setMethod("CARD");
            refreshAutoPrint();
            setStage("attract");
          }}
          onBack={() => {
            refreshAutoPrint();
            setStage(adminFrom);
          }}
          onVerticalChange={onLeave}
        />
        {error ? <Toast message={error} /> : null}
      </div>
    );
  }

  if (stage === "loading") {
    return (
      <div className="k-app">
        <Splash message="Abriendo la caja…" />
      </div>
    );
  }

  if (stage === "attract") {
    return (
      <div className="k-app">
        <RetailAttractScreen onBrowse={browse} />
        <CameraCorner enabled={cameraPreview} vertical="RETAIL" />
        <button
          type="button"
          className="k-admin-key"
          onClick={() => openAdmin("attract")}
        >
          <span aria-hidden>⚙</span> Admin
        </button>
        {error ? <Toast message={error} /> : null}
      </div>
    );
  }

  return (
    <div className="k-app">
      {/* One element for every screen in this lane, so moving between them
          does not stop and restart the camera. Admin is not in this tree:
          the operator's screen has no customer to show. */}
      <CameraCorner
        enabled={cameraPreview}
        vertical="RETAIL"
        hidden={stage === "search"}
      />
      {stage === "lane" ? (
        <>
          <Header
            cartCount={0}
            title="Tu compra"
            backLabel="Cancelar"
            onBack={() => void clear(true)}
          />
          <main
            className={`k-main k-fade k-r-main${
              cameraPreview ? " k-r-main--cam" : ""
            }`}
          >
            <RetailCartScreen
              transaction={transaction}
              busy={busy}
              onQuantity={(line, quantity) =>
                void mutate(() => changeQuantity(line, quantity))
              }
              onRemove={(line) => void mutate(() => removeItem(line))}
            />
            <RetailTotalPanel
              transaction={transaction}
              busy={busy}
              onPay={() => void checkout()}
              onBrowse={browse}
            />
          </main>
        </>
      ) : null}

      {stage === "search" && menu ? (
        <>
          <Header
            cartCount={units}
            cartTotal={transaction?.running_totals.grand_total}
            backLabel={units > 0 ? "Volver a mi compra" : "Inicio"}
            // Going home with nothing in the bag means the customer left, so
            // the empty sale the lane may have opened goes with them.
            onBack={() => (units > 0 ? setStage("lane") : void clear(true))}
            onCart={() => setStage("lane")}
          />
          <Screen>
            <RetailSearchScreen
              menu={menu}
              busy={busy}
              onAdd={(item) => void take(item)}
              onBarcode={(barcode) => void typed(barcode)}
            />
          </Screen>
        </>
      ) : null}

      {stage === "method" && transaction ? (
        <>
          <Header
            cartCount={0}
            backLabel="Volver a mi compra"
            onBack={() => setStage("lane")}
          />
          <Screen>
            <MethodScreen
              due={transaction.totals.grand_total}
              onPick={(picked) => {
                setMethod(picked);
                resetPaying();
                setStage("payment");
              }}
            />
          </Screen>
        </>
      ) : null}

      {stage === "payment" && transaction ? (
        <>
          <Header
            cartCount={0}
            backLabel="Cambiar método"
            onBack={paying === "waiting" ? () => setStage("method") : undefined}
          />
          <Screen>
            {method === "CASH" ? (
              <CashScreen due={transaction.totals.grand_total} state={paying} />
            ) : (
              <PaymentScreen transaction={transaction} state={paying} />
            )}
          </Screen>
        </>
      ) : null}

      {stage === "done" && confirmed ? (
        <>
          <header className="k-header">
            <Brand />
          </header>
          <Screen>
            <ConfirmationScreen
              orderCode={confirmed.order}
              fulfillment="SELF_CHECKOUT"
              total={confirmed.total}
              method={confirmed.method}
              seconds={countdown}
              of={CONFIRM_SECONDS}
            />
          </Screen>
        </>
      ) : null}

      {error ? <Toast message={error} /> : null}
    </div>
  );
}
