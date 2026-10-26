/**
 * PX Kiosk — the self-service ordering screen.
 *
 * One state machine over the screens a customer walks through:
 *
 * ```text
 * attract → fulfillment → menu ⇄ product → cart → method → payment → done ┐
 *     ↑                                                                   │
 *     └──────────────── idle timeout, or the confirmation ends ───────────┘
 * ```
 *
 * The sale itself lives in the core, not here: navigating between screens
 * cannot lose it, and a reload picks the open sale back up. The only thing
 * this component owns is which screen is showing and what the customer has
 * tapped but not yet sent (ADR 002, ADR 046).
 */
import { useCallback, useEffect, useRef, useState } from "react";

import { AdminScreen } from "./admin";
import {
  addItem,
  activeTransaction,
  cancelTransaction,
  changeQuantity,
  describe,
  fetchConfiguration,
  fetchMenu,
  fetchPrinterSettings,
  pay,
  printReceipt,
  removeItem,
  startTransaction,
  totalize,
} from "./api";
import {
  Brand,
  ErrorBoundary,
  Header,
  Screen,
  Splash,
  Steps,
  Toast,
} from "./components";
import {
  AttractScreen,
  CartScreen,
  CashScreen,
  ConfirmationScreen,
  FulfillmentScreen,
  MenuScreen,
  MethodScreen,
  PaymentScreen,
  PostAddScreen,
  ProductScreen,
  recommendationsFor,
  type Fulfillment,
  type Method,
} from "./screens";
import type {
  Menu,
  MenuItem,
  ProductConfiguration,
  RequestedSelection,
  RetailTransaction,
} from "./types";
import "./kiosk.css";

/** The way of selling this screen is. The catalog decides what that offers. */
const CHANNEL = "KIOSK";

/** How long a screen may sit untouched before the order is abandoned. */
const IDLE_MS = 75_000;

/**
 * How long the confirmation stays up before the attract screen returns.
 *
 * Four seconds: long enough to read a two-part order number out loud, short
 * enough that the next customer is never waiting on the screen.
 *
 * The `satisfies 4` is the guard, not decoration. There is no test runner on
 * this side of the wire, so the type is what holds the number down: write ten
 * here and `tsc --noEmit` fails before anyone sees a kiosk sit on a finished
 * order.
 */
const CONFIRM_SECONDS = 4 satisfies 4;

type Stage =
  | "loading"
  | "attract"
  | "fulfillment"
  | "menu"
  | "product"
  | "postadd"
  | "cart"
  | "admin"
  | "method"
  | "payment"
  | "done";

/**
 * A render failure must never be a black screen.
 *
 * The boundary shows what broke and remounts the kiosk on demand. The sale
 * itself lives in the core, so a remount picks it straight back up.
 */
export default function KioskApp() {
  const [generation, setGeneration] = useState(0);
  return (
    <ErrorBoundary
      key={generation}
      onReset={() => setGeneration((value) => value + 1)}
    >
      <Kiosk />
    </ErrorBoundary>
  );
}

function Kiosk() {
  const [stage, setStage] = useState<Stage>("loading");
  const [menu, setMenu] = useState<Menu | null>(null);
  const [fulfillment, setFulfillment] = useState<Fulfillment>("DINE_IN");
  const [category, setCategory] = useState<string | null>(null);
  const [item, setItem] = useState<MenuItem | null>(null);
  const [configuration, setConfiguration] =
    useState<ProductConfiguration | null>(null);
  const [transaction, setTransaction] = useState<RetailTransaction | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [paying, setPaying] = useState<"waiting" | "approving">("waiting");
  const [method, setMethod] = useState<Method>("CARD");
  /** Where to go once this product is added: the menu, or back to the cart. */
  const [returnTo, setReturnTo] = useState<"menu" | "cart">("menu");
  /** The dish the "Completa tu pedido" step is offering alongside. */
  const [justAdded, setJustAdded] = useState<MenuItem | null>(null);
  /** Whether this kiosk was told to put a receipt out by itself. */
  const [autoPrint, setAutoPrint] = useState(false);
  /** Where Admin came from, so leaving it puts the screen back. */
  const [adminFrom, setAdminFrom] = useState<Stage>("attract");
  const [confirmed, setConfirmed] = useState<{
    order: string;
    total: string;
    method: Method;
  } | null>(null);
  const [countdown, setCountdown] = useState(CONFIRM_SECONDS);

  const stageRef = useRef(stage);
  stageRef.current = stage;

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

  // --- the menu, once, from the snapshot in force ------------------------
  useEffect(() => {
    let live = true;
    void fetchMenu(CHANNEL)
      .then((value) => {
        if (!live) {
          return;
        }
        setMenu(value);
        setStage("attract");
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
  }, [complain]);

  // --- does this kiosk print its own receipts? ---------------------------
  //
  // Asked of the core, never remembered by the screen: an operator can change
  // it on the admin screen and the next sale has to honour the new answer.
  const refreshAutoPrint = useCallback(() => {
    void fetchPrinterSettings()
      .then((config) => setAutoPrint(config.settings.enabled && config.settings.auto_print))
      .catch(() => setAutoPrint(false));
  }, []);

  useEffect(refreshAutoPrint, [refreshAutoPrint]);

  // --- abandon a forgotten order rather than leave it on screen ----------
  const reset = useCallback(
    async (cancel: boolean) => {
      if (cancel) {
        try {
          await cancelTransaction("KIOSK_ABANDONED");
        } catch {
          // Nothing to abandon, or it already closed. Either way the screen
          // goes home: the core is the one that knows, and it just said so.
        }
      }
      setTransaction(null);
      setItem(null);
      setJustAdded(null);
      setConfiguration(null);
      setCategory(null);
      setConfirmed(null);
      setPaying("waiting");
      setMethod("CARD");
      setReturnTo("menu");
      setStage("attract");
    },
    [],
  );

  useEffect(() => {
    if (stage === "attract" || stage === "loading" || stage === "done") {
      return;
    }
    let timer = 0;
    const arm = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        void reset(true);
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
  }, [stage, reset]);

  // --- the confirmation counts itself down -------------------------------
  useEffect(() => {
    if (stage !== "done") {
      return;
    }
    setCountdown(CONFIRM_SECONDS);
    const timer = window.setInterval(() => {
      setCountdown((value) => {
        if (value <= 1) {
          window.clearInterval(timer);
          void reset(false);
          return 0;
        }
        return value - 1;
      });
    }, 1000);
    return () => window.clearInterval(timer);
  }, [stage, reset]);

  // --- actions ------------------------------------------------------------

  const begin = () => {
    setStage("fulfillment");
  };

  const choose = async (choice: Fulfillment) => {
    setFulfillment(choice);
    setBusy(true);
    try {
      // Pick an order already open on this register back up rather than
      // refusing: the core is the one that knows there is one.
      const existing = await activeTransaction();
      setTransaction(existing ?? (await startTransaction()));
      setStage("menu");
    } catch (reason) {
      complain(reason);
    } finally {
      setBusy(false);
    }
  };

  const open = async (
    picked: MenuItem,
    from: "menu" | "cart" | "postadd" = "menu",
  ) => {
    setItem(picked);
    // A suggestion taken from the offer screen comes back to the basket, not
    // to another offer: the customer is told once, not in a loop.
    setReturnTo(from === "menu" ? "menu" : "cart");
    setBusy(true);
    try {
      const value = await fetchConfiguration(picked.product_ref, CHANNEL);
      setConfiguration(value);
      if (value.groups.length === 0) {
        // Nothing to ask: one tap adds it, and the screen does not move.
        const updated = await addItem(picked.product_ref, 1, [], null);
        setTransaction(updated);
        setItem(null);
        setConfiguration(null);
        return;
      }
      setStage("product");
    } catch (reason) {
      complain(reason);
      setItem(null);
    } finally {
      setBusy(false);
    }
  };

  const add = async (selections: RequestedSelection[], quantity: number) => {
    if (!item) {
      return;
    }
    setBusy(true);
    try {
      const updated = await addItem(
        item.product_ref,
        quantity,
        selections,
        null,
      );
      setTransaction(updated);
      const added = item;
      setItem(null);
      setConfiguration(null);
      if (returnTo === "menu" && menu) {
        const offers = recommendationsFor(
          menu,
          [added.product_ref],
          updated.lines.map((line) => line.product_ref),
        );
        if (offers.length > 0) {
          // Offer once, between the dish and the basket. A dish the catalog
          // recommends nothing for skips the step rather than showing an
          // empty screen.
          setJustAdded(added);
          setStage("postadd");
          return;
        }
      }
      // A suggestion that needed configuring returns to the cart it came from.
      setStage(returnTo);
      setReturnTo("menu");
    } catch (reason) {
      complain(reason);
    } finally {
      setBusy(false);
    }
  };

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

  const checkout = async () => {
    setBusy(true);
    try {
      const totalized = await totalize();
      setTransaction(totalized);
      setPaying("waiting");
      setStage("method");
    } catch (reason) {
      complain(reason);
    } finally {
      setBusy(false);
    }
  };

  const choosePayment = (picked: Method) => {
    setMethod(picked);
    setPaying("waiting");
    setStage("payment");
  };

  // --- payment, as a demo terminal would feel ----------------------------
  useEffect(() => {
    if (stage !== "payment" || !transaction) {
      return;
    }
    const tap = window.setTimeout(
      () => setPaying("approving"),
      method === "CASH" ? 2200 : 2600,
    );
    return () => window.clearTimeout(tap);
  }, [stage, transaction, method]);

  useEffect(() => {
    if (stage !== "payment" || paying !== "approving" || !transaction) {
      return;
    }
    let live = true;
    const run = window.setTimeout(() => {
      // The amount is the core's own grand total, handed straight back, and
      // the tender is the one the customer chose. The screen never decides
      // what is owed and never decides it was paid.
      void pay(method, transaction.totals.grand_total.minor)
        .then((finalized) => {
          if (!live) {
            return;
          }
          setConfirmed({
            order: finalized.order_code,
            total: `$${finalized.totals.grand_total.formatted}`,
            method,
          });
          setTransaction(null);
          setStage("done");
          // Paper is a second act, after the money. The sale is finalized and
          // durable by the time this runs, so a printer that is off, jammed
          // or unplugged cannot touch it — the attempt resolves to a boolean
          // and the core records the receipt's own fate either way.
          if (autoPrint) {
            void printReceipt(finalized.transaction_id).then((printed) => {
              if (!printed) {
                // Worth knowing at the counter, not worth alarming a customer
                // whose order is already paid for and on its way.
                console.warn("the receipt did not print");
              }
            });
          }
        })
        .catch((reason) => {
          if (live) {
            complain(reason);
            setPaying("waiting");
            setStage("cart");
          }
        });
    }, 1300);
    return () => {
      live = false;
      window.clearTimeout(run);
    };
  }, [stage, paying, transaction, method, complain, autoPrint]);

  // --- render -------------------------------------------------------------

  const count = (transaction?.lines ?? []).reduce(
    (sum, line) => sum + Math.max(1, Math.round(line.quantity_thousandths / 1000)),
    0,
  );
  const cartTotal = transaction?.running_totals.grand_total;

  if (stage === "admin") {
    return (
      <div className="k-app">
        <AdminScreen
          saleOpen={transaction !== null}
          onReset={() => {
            // The core already abandoned the order; clear what the screen
            // was holding and go home.
            setTransaction(null);
            setItem(null);
            setJustAdded(null);
            setConfiguration(null);
            setCategory(null);
            setConfirmed(null);
            setPaying("waiting");
            setMethod("CARD");
            setReturnTo("menu");
            refreshAutoPrint();
            setStage("attract");
          }}
          onBack={() => {
            // Leaving admin never touches a sale: the screen goes back to
            // exactly where it came from, and the core kept the order.
            refreshAutoPrint();
            setStage(adminFrom);
          }}
        />
        {error ? <Toast message={error} /> : null}
      </div>
    );
  }

  if (stage === "loading") {
    return (
      <div className="k-app">
        <Splash message="Preparando el kiosko…" />
      </div>
    );
  }

  if (stage === "attract") {
    // The flagship category's photograph doubles as the attract banner.
    const hero =
      menu?.categories[0]?.image ??
      menu?.items.find((entry) => entry.image)?.image ??
      null;
    return (
      <div className="k-app">
        <AttractScreen heroAsset={hero} onStart={begin} />
        <button
          type="button"
          className="k-admin-key"
          onClick={() => {
            setAdminFrom("attract");
            setStage("admin");
          }}
        >
          <span aria-hidden>⚙</span> Admin
        </button>
        {error ? <Toast message={error} /> : null}
      </div>
    );
  }

  return (
    <div className="k-app">
      {stage === "fulfillment" ? (
        <>
          <Header cartCount={0} onBack={() => void reset(true)} backLabel="Inicio" />
          <Screen>
            <FulfillmentScreen onPick={(choice) => void choose(choice)} />
          </Screen>
        </>
      ) : null}

      {stage === "menu" && menu ? (
        <>
          <Header
            cartCount={count}
            cartTotal={cartTotal}
            onCart={() => setStage("cart")}
          />
          <Screen>
            <MenuScreen
              menu={menu}
              activeCategory={category}
              onCategory={setCategory}
              onPick={(picked) => void open(picked)}
            />
          </Screen>
          <footer className="k-bar">
            <button
              type="button"
              className="k-btn k-btn--quiet"
              onClick={() => void reset(true)}
            >
              Cancelar pedido
            </button>
            <Steps at={2} />
            <div className="k-header__spacer" />
            <button
              type="button"
              className="k-btn k-btn--primary k-btn--lg"
              disabled={count === 0}
              onClick={() => setStage("cart")}
            >
              Ver mi pedido
              {cartTotal ? ` · $${cartTotal.formatted}` : ""}
              <span aria-hidden>→</span>
            </button>
          </footer>
        </>
      ) : null}

      {stage === "product" && item && configuration ? (
        <>
          <Header
            cartCount={count}
            cartTotal={cartTotal}
            backLabel="Menú"
            onBack={() => {
              setItem(null);
              setConfiguration(null);
              setStage("menu");
            }}
            onCart={() => setStage("cart")}
          />
          <Screen>
            <ProductScreen
              item={item}
              configuration={configuration}
              busy={busy}
              onAdd={(selections, quantity) => void add(selections, quantity)}
            />
          </Screen>
        </>
      ) : null}

      {stage === "postadd" && menu && justAdded ? (
        <>
          <Header
            cartCount={count}
            cartTotal={cartTotal}
            backLabel="Menú"
            onBack={() => {
              setJustAdded(null);
              setStage("menu");
            }}
            onCart={() => {
              setJustAdded(null);
              setStage("cart");
            }}
          />
          <Screen>
            <PostAddScreen
              item={justAdded}
              suggestions={recommendationsFor(
                menu,
                [justAdded.product_ref],
                (transaction?.lines ?? []).map((line) => line.product_ref),
              )}
              busy={busy}
              onAdd={(picked) => {
                setJustAdded(null);
                void open(picked, "postadd");
              }}
              onSkip={() => {
                setJustAdded(null);
                setStage("cart");
              }}
            />
          </Screen>
        </>
      ) : null}

      {stage === "cart" && menu ? (
        <>
          <Header
            cartCount={count}
            backLabel="Seguir pidiendo"
            onBack={() => setStage("menu")}
            title="Tu pedido"
          />
          <Screen>
            <CartScreen
              transaction={transaction}
              menu={menu}
              busy={busy}
              onQuantity={(line, quantity) =>
                void mutate(() => changeQuantity(line, quantity))
              }
              onRemove={(line) => void mutate(() => removeItem(line))}
              onKeepOrdering={() => setStage("menu")}
              onSuggest={(picked) => void open(picked, "cart")}
            />
          </Screen>
          {count > 0 ? (
            <footer className="k-bar">
              <button
                type="button"
                className="k-btn k-btn--quiet"
                onClick={() => void reset(true)}
              >
                Cancelar pedido
              </button>
              <div className="k-header__spacer" />
              <button
                type="button"
                className="k-btn k-btn--primary k-btn--lg"
                disabled={busy}
                onClick={() => void checkout()}
              >
                Pagar
                {cartTotal ? ` · $${cartTotal.formatted}` : ""}
                <span aria-hidden>→</span>
              </button>
            </footer>
          ) : null}
        </>
      ) : null}

      {stage === "method" && transaction ? (
        <>
          <Header
            cartCount={0}
            backLabel="Volver al pedido"
            onBack={() => setStage("cart")}
          />
          <Screen>
            <MethodScreen
              due={transaction.totals.grand_total}
              onPick={choosePayment}
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
            fulfillment={fulfillment}
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
