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
import { CameraCorner } from "./CameraCorner";
import {
  addItem,
  activeTransaction,
  cancelTransaction,
  changeQuantity,
  describe,
  fetchConfiguration,
  fetchMenu,
  fetchPrinterSettings,
  removeItem,
  startTransaction,
  totalize,
  paymentAllowed,
} from "./api";
import type { Invitation } from "./brands";
import {
  Gate,
  screenAfterPick,
  showsClosedSign,
  type Origin,
  type Stage,
} from "./flow";
import { usePaymentJourney } from "./payment";
import {
  Brand,
  ClosedScreen,
  ErrorBoundary,
  Header,
  Screen,
  Splash,
  Steps,
  Toast,
  PrinterBlockedScreen,
} from "./components";
import {
  AttractScreen,
  CartScreen,
  CashGenicScreen,
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

/**
 * A render failure must never be a black screen.
 *
 * The boundary shows what broke and remounts the kiosk on demand. The sale
 * itself lives in the core, so a remount picks it straight back up.
 */
export default function KioskApp({
  channel,
  brandName,
  kioskOpen,
  cashGenic,
  cameraPreview,
  invitation,
  onLeave,
}: {
  /** The catalog channel this brand sells from. Chosen by the host. */
  channel: string;
  /** Whose menu this is, for the screens that say so. */
  brandName: string;
  /** Whether the kiosk is taking orders at all. */
  kioskOpen: boolean;
  /** Whether cash is taken by a real note acceptor. */
  cashGenic: boolean;
  /** Whether this box shows the customer their own camera. */
  cameraPreview: boolean;
  /** What this brand's attract screen says. */
  invitation: Invitation;
  /** Called when an operator switched the installation to another vertical. */
  onLeave: () => void;
}) {
  const [generation, setGeneration] = useState(0);
  return (
    <ErrorBoundary
      key={generation}
      onReset={() => setGeneration((value) => value + 1)}
    >
      <Kiosk
        channel={channel}
        brandName={brandName}
        kioskOpen={kioskOpen}
        cashGenic={cashGenic}
        cameraPreview={cameraPreview}
        invitation={invitation}
        onLeave={onLeave}
      />
    </ErrorBoundary>
  );
}

function Kiosk({
  channel,
  brandName,
  kioskOpen,
  cashGenic,
  cameraPreview,
  invitation,
  onLeave,
}: {
  channel: string;
  brandName: string;
  kioskOpen: boolean;
  cashGenic: boolean;
  cameraPreview: boolean;
  invitation: Invitation;
  onLeave: () => void;
}) {
  const CHANNEL = channel;
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
  const [method, setMethod] = useState<Method>("CARD");
  /**
   * Why the printer is stopping this sale, when it is.
   *
   * Held rather than thrown: the customer is standing there and the lane has
   * to say what is wrong and offer to look again, not fail a tender choice.
   */
  const [printerBlock, setPrinterBlock] = useState<string | null>(null);
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

  /** Held while a pick is in flight, so one tap is one line. */
  const picking = useRef(new Gate());

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
  }, [CHANNEL, complain]);

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

  // --- payment, as a demo terminal would feel ----------------------------
  //
  // The journey itself is shared with the retail lane: one hook, one `pay`
  // call, one place where a tender becomes a finalized sale.
  const { paying, acceptor, reset: resetPaying, cancel: cancelCash } =
    usePaymentJourney({
    active: stage === "payment",
    transaction,
    method,
    autoPrint,
    cashGenic,
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
      setStage("cart");
    },
  });

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
      resetPaying();
      setMethod("CARD");
      setPrinterBlock(null);
      setReturnTo("menu");
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
    // The closed sign is checked here as well as drawn: a tap that arrived
    // while the screen was changing must not start an order either.
    if (!kioskOpen) {
      return;
    }
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

  /**
   * What the catalog says to offer alongside a dish just added.
   *
   * The list is the catalog's — `recommended_product_refs` — narrowed to what
   * this menu shows and to what the basket does not already hold. Used by
   * both add paths, so a lane with no configurable products gets the same
   * cross-sell as one with them (ADR 046).
   */
  const offersFor = (added: MenuItem, sale: RetailTransaction) =>
    menu
      ? recommendationsFor(
          menu,
          [added.product_ref],
          sale.lines.map((line) => line.product_ref),
        )
      : [];

  /** Put the screen where `flow.ts` says it goes, with what it needs. */
  const show = (screen: { stage: Stage; offer: boolean }, picked: MenuItem) => {
    setJustAdded(screen.offer ? picked : null);
    setStage(screen.stage);
  };

  const open = async (picked: MenuItem, from: Origin = "menu") => {
    // A second tap on the same card while the first is in flight would be a
    // second sale line. Refused here, where the tap is, because `busy` only
    // reaches the buttons on the next render (ADR 046).
    if (!picking.current.enter()) {
      return;
    }
    setItem(picked);
    // A suggestion taken from the offer screen comes back to the basket, not
    // to another offer: the customer is told once, not in a loop.
    setReturnTo(from === "menu" ? "menu" : "cart");
    setBusy(true);
    try {
      const value = await fetchConfiguration(picked.product_ref, CHANNEL);
      if (value.groups.length > 0) {
        setConfiguration(value);
        show(screenAfterPick(from, "configure"), picked);
        return;
      }
      // Nothing to ask: one tap adds it. Where that leaves the screen depends
      // on where the tap came from — and it is never "wherever it was", which
      // is what used to leave the offer screen rendering nothing at all.
      const updated = await addItem(picked.product_ref, 1, [], null);
      setTransaction(updated);
      setItem(null);
      setConfiguration(null);
      if (from === "menu" && offersFor(picked, updated).length > 0) {
        // A dish that needs no configuring still has things to offer
        // alongside it. Lanes where *nothing* is configurable — a coffee
        // shop, a pizza counter — would otherwise never see the offer screen
        // at all, which is where the cross-sell lives.
        setJustAdded(picked);
        setStage("postadd");
        return;
      }
      show(screenAfterPick(from, "added"), picked);
    } catch (reason) {
      complain(reason);
      setItem(null);
      setConfiguration(null);
      show(screenAfterPick(from, "failed"), picked);
    } finally {
      setBusy(false);
      picking.current.leave();
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
        const offers = offersFor(added, updated);
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
      resetPaying();
      // Asked again on the way in, by `choosePayment`. A notice left over
      // from the last attempt would hide a printer somebody has since fixed.
      setPrinterBlock(null);
      setStage("method");
    } catch (reason) {
      complain(reason);
    } finally {
      setBusy(false);
    }
  };

  /**
   * Take the customer to the payment screen — unless the printer cannot
   * follow it with a receipt.
   *
   * The question is put to the host before the screen changes, which is the
   * whole point: nobody should reach a payment screen for a ticket that
   * cannot come out. The host refuses the payment command too, so this is the
   * courteous half of a rule that holds either way.
   */
  const choosePayment = async (picked: Method) => {
    setBusy(true);
    try {
      const gate = await paymentAllowed();
      if (!gate.allowed) {
        // A counter lane can only ever be stopped by the printer — the
        // authorization flag is a retail concept, and the host answers
        // nothing for the other three brands. Whatever the reason, the host's
        // own words go on the screen.
        setPrinterBlock(gate.message);
        return;
      }
      setMethod(picked);
      setPrinterBlock(null);
      resetPaying();
      setStage("payment");
    } catch (reason) {
      complain(reason);
    } finally {
      setBusy(false);
    }
  };

  /** Look again, after somebody has closed the cover or changed the roll. */
  const recheckPrinter = async () => {
    setBusy(true);
    try {
      const gate = await paymentAllowed();
      setPrinterBlock(gate.allowed ? null : gate.message);
    } catch (reason) {
      setPrinterBlock(describe(reason));
    } finally {
      setBusy(false);
    }
  };

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
            resetPaying();
            setMethod("CARD");
            setPrinterBlock(null);
      setPrinterBlock(null);
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
          onVerticalChange={onLeave}
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

  // One element for every screen in this lane, so moving between them does
  // not stop and restart the camera. Admin is not in this tree: the
  // operator's screen has no customer to show.
  const corner = (
    <CameraCorner
      enabled={cameraPreview}
      hidden={stage === "product" || stage === "postadd"}
    />
  );

  if (stage === "attract") {
    // The flagship category's photograph doubles as the attract banner.
    const hero =
      menu?.categories[0]?.image ??
      menu?.items.find((entry) => entry.image)?.image ??
      null;
    return (
      <div className="k-app">
        {showsClosedSign(kioskOpen, stage) ? (
          <ClosedScreen brandName={brandName} />
        ) : (
          <AttractScreen
            heroAsset={hero}
            invitation={invitation}
            onStart={begin}
          />
        )}
        {corner}
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
      {corner}
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
              // The dish stays set until the flow says the screen has
              // somewhere else to be: clearing it here was what left this
              // screen with nothing to draw.
              onAdd={(picked) => void open(picked, "postadd")}
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
            {printerBlock ? (
              <PrinterBlockedScreen
                message={printerBlock}
                busy={busy}
                onRetry={() => void recheckPrinter()}
              />
            ) : (
              <MethodScreen
                due={transaction.totals.grand_total}
                onPick={(picked) => void choosePayment(picked)}
              />
            )}
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
              cashGenic ? (
                <CashGenicScreen
                  due={transaction.totals.grand_total}
                  status={acceptor}
                  onCancel={() => {
                    cancelCash();
                    setStage("cart");
                  }}
                />
              ) : (
                <CashScreen due={transaction.totals.grand_total} state={paying} />
              )
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
