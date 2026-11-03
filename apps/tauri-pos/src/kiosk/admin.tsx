/**
 * The kiosk's own settings screen.
 *
 * Local, and only local: it configures the box on this desk. There is no
 * account, no cloud and no remote config, and the one thing it can change —
 * which receipt printer this installation drives — is stored and validated by
 * the core, not here. The screen renders what the core returned and sends
 * back what the operator chose (ADR 039).
 */
import { useCallback, useEffect, useState } from "react";

import {
  closeDay,
  dayTally,
  describe,
  fetchPrinterSettings,
  openDay,
  preferences,
  probeCashGenic,
  resetDemo,
  saveCashGenic,
  setCashGenicEnabled,
  printerStatus,
  savePrinterSettings,
  setCameraPreview,
  setIdentity,
  setKioskOpen,
  setVertical,
  testPrint,
  adminProducts,
  saveProducts,
  setPin,
  setRemoteControl,
} from "./api";
import { majorOf, minorOf } from "./flow";
import { BRANDS, brandOf } from "./brands";
import { Brand, PoweredBy, AssetImage } from "./components";
import {
  CameraPreview,
  browserCamera,
  describeCamera,
  type CameraStatus,
} from "./camera";
import type {
  CashGenicSettings,
  CashGenicStatus,
  DayTally,
  KioskPreferences,
  PrinterConfiguration,
  PrinterSettings,
  PrinterStatus,
  Vertical,
  AdminProduct,
  ProductEdit,
} from "./types";

/** Minor units as a peso amount. Display only; the core owns every total. */
function pesos(minor: number): string {
  const sign = minor < 0 ? "-" : "";
  const abs = Math.abs(minor);
  return `${sign}${Math.floor(abs / 100)}.${String(abs % 100).padStart(2, "0")}`;
}

type Note = { tone: "ok" | "bad"; text: string } | null;

export function AdminScreen({
  saleOpen,
  onBack,
  onReset,
  onVerticalChange,
}: {
  /** A sale in progress. Admin never touches one, so it says so instead. */
  saleOpen: boolean;
  onBack: () => void;
  /** Called once the open order has been abandoned, to go back to attract. */
  onReset: () => void;
  /**
   * Called when the installation was switched to the other vertical, so the
   * shell can mount the lane that was chosen. The sale, if there was one, is
   * already abandoned by then: the host does that on the way through.
   */
  onVerticalChange: (preferences: KioskPreferences) => void;
}) {
  const [config, setConfig] = useState<PrinterConfiguration | null>(null);
  const [lane, setLane] = useState<KioskPreferences | null>(null);
  const [camera, setCamera] = useState<CameraStatus>("off");
  const [day, setDay] = useState<DayTally | null>(null);
  /** Identity as it is being typed, before it is saved. */
  const [who, setWho] = useState({ branch: "", kiosk: "", cashier: "" });
  const [cash, setCash] = useState<CashGenicSettings | null>(null);
  const [acceptor, setAcceptor] = useState<CashGenicStatus | null>(null);
  /** The figures shown before "cerrar día" is confirmed. */
  const [closing, setClosing] = useState(false);
  const [draft, setDraft] = useState<PrinterSettings | null>(null);
  /** What the printer last said about itself. */
  const [paper, setPaper] = useState<PrinterStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<Note>(null);

  const load = useCallback(async () => {
    try {
      const value = await fetchPrinterSettings();
      setConfig(value);
      setDraft(value.settings);
    } catch (reason) {
      setNote({ tone: "bad", text: describe(reason) });
    }
    // The cached answer: the core refreshes it every fifteen seconds, and an
    // operator who wants it asked *now* has a button for that.
    try {
      setPaper(await printerStatus(false));
    } catch {
      // A runtime that drives no printer cannot answer, and the panel says
      // "sin canal de estado" rather than showing an error for it.
      setPaper(null);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    void preferences()
      .then((stored) => {
        setLane(stored);
        setWho({
          branch: stored.branch_id,
          kiosk: stored.kiosk_id,
          cashier: stored.cashier_id,
        });
        setCash(stored.cashgenic);
      })
      .catch((reason: unknown) => setNote({ tone: "bad", text: describe(reason) }));
    void dayTally()
      .then(setDay)
      .catch((reason: unknown) => setNote({ tone: "bad", text: describe(reason) }));
  }, []);

  /** One place to run an Admin action and report what came back. */
  const act = async (work: () => Promise<void>, done?: string) => {
    setBusy(true);
    try {
      await work();
      setNote(done ? { tone: "ok", text: done } : null);
    } catch (reason) {
      setNote({ tone: "bad", text: describe(reason) });
    } finally {
      setBusy(false);
    }
  };

  /**
   * The "Habilitar CashGenic" switch, saved on the spot.
   *
   * The switch shows what the file says, never a wish: nothing changes on
   * screen until the host has written it, and a refusal puts it back where the
   * file still has it. The address and credentials being typed are left as
   * they are — "Guardar" is still what saves those.
   */
  const switchCashGenic = async (wanted: boolean) => {
    const persisted = lane?.cashgenic.enabled ?? !wanted;
    setBusy(true);
    try {
      const saved = await setCashGenicEnabled(wanted);
      setLane(saved);
      setCash((current) =>
        current
          ? { ...current, enabled: saved.cashgenic.enabled }
          : saved.cashgenic,
      );
      setNote({
        tone: "ok",
        text: saved.cashgenic.enabled
          ? "CashGenic habilitado y guardado."
          : "CashGenic deshabilitado y guardado.",
      });
    } catch (reason) {
      setCash((current) =>
        current ? { ...current, enabled: persisted } : current,
      );
      setNote({
        tone: "bad",
        text: `No se guardó: CashGenic sigue ${persisted ? "habilitado" : "deshabilitado"}. ${describe(reason)}`,
      });
    } finally {
      setBusy(false);
    }
  };

  /**
   * Find out whether the camera this box was told to use actually opens.
   *
   * Run only when the preview is on, and only on this screen: the operator
   * has just asked for it, and "it is enabled but the machine refuses" is
   * exactly what they need to be told here rather than discover on a stand.
   * The stream is closed the moment it has answered — Admin shows no preview,
   * and nothing is recorded, stored or sent anywhere (ADR 047).
   */
  useEffect(() => {
    if (!lane?.camera_preview) {
      setCamera("off");
      return;
    }
    const probe = new CameraPreview(browserCamera());
    let live = true;
    void probe.start().then((status) => {
      if (live) {
        setCamera(status);
      }
      probe.stop();
    });
    return () => {
      live = false;
      probe.stop();
    };
  }, [lane?.camera_preview]);

  const toggleCamera = async (enabled: boolean) => {
    setBusy(true);
    try {
      setLane(await setCameraPreview(enabled));
      setNote(null);
    } catch (reason) {
      setNote({ tone: "bad", text: describe(reason) });
    } finally {
      setBusy(false);
    }
  };

  /**
   * Switch the installation to the other lane.
   *
   * Nothing is re-imported and nothing is re-activated: both verticals live
   * in the one active catalog and are told apart by channel, so this is a
   * change of what the screen reads (ADR 044). The open basket is abandoned
   * by the host on the way through — deliberately, because a half-built
   * order must not reappear in a shop that does not sell it — and the printer
   * settings, the photographs and the till session all survive it.
   */
  const switchLane = async (vertical: Vertical) => {
    if (lane?.vertical === vertical) {
      return;
    }
    setBusy(true);
    try {
      const chosen = await setVertical(vertical);
      setLane(chosen);
      onVerticalChange(chosen);
    } catch (reason) {
      setNote({ tone: "bad", text: describe(reason) });
      setBusy(false);
    }
  };

  // --- products and prices ------------------------------------------------
  //
  // The list is the **core's own menu** for whichever experience is being
  // looked at, with this kiosk's edits laid over it. Admin can browse an
  // experience it is not currently running — somebody setting up a stand
  // prices all four — so the shelf being shown is its own piece of state,
  // separate from the lane in use.

  /** Which experience's shelf is on screen. Defaults to the one in use. */
  const [shelf, setShelf] = useState<Vertical | null>(null);
  const [products, setProducts] = useState<AdminProduct[] | null>(null);
  /**
   * Prices as they are being typed, by `product_ref`.
   *
   * Held apart from `products` so the list keeps showing what the till
   * charges while a number is half-typed, and so [Aplicar precios] knows
   * exactly what changed.
   */
  const [typed, setTyped] = useState<Record<string, string>>({});
  /** Authorization flags as they are being switched, by `product_ref`. */
  const [flags, setFlags] = useState<Record<string, boolean>>({});

  const shelfChannel = shelf ? brandOf(shelf).channel : null;

  const loadProducts = useCallback(async (channel: string) => {
    try {
      setProducts(await adminProducts(channel));
      setTyped({});
      setFlags({});
    } catch (reason) {
      setProducts([]);
      setNote({ tone: "bad", text: describe(reason) });
    }
  }, []);

  // The lane in use decides which shelf opens, once.
  useEffect(() => {
    if (lane && shelf === null) {
      setShelf(lane.vertical);
    }
  }, [lane, shelf]);

  useEffect(() => {
    if (shelfChannel) {
      void loadProducts(shelfChannel);
    }
  }, [shelfChannel, loadProducts]);

  /** What an operator has actually changed, ready to send. */
  const edits: ProductEdit[] = (products ?? [])
    .filter((product) => {
      const price = typed[product.product_ref];
      const flag = flags[product.product_ref];
      const pricePending =
        price !== undefined && price.trim() !== "" && minorOf(price) !== product.price_minor;
      const flagPending = flag !== undefined && flag !== product.authorization_required;
      return pricePending || flagPending;
    })
    .map((product) => {
      const price = typed[product.product_ref];
      return {
        product_ref: product.product_ref,
        price_minor:
          price !== undefined && price.trim() !== ""
            ? minorOf(price)
            : product.price_overridden
              ? product.price_minor
              : null,
        authorization_required:
          flags[product.product_ref] ?? product.authorization_required,
      };
    });

  const badPrices = Object.entries(typed).filter(
    ([, value]) => value.trim() !== "" && minorOf(value) === null,
  );

  /**
   * Write the edits and rebuild the catalog.
   *
   * One press for the whole batch, because each price has to reach the
   * catalog in force and the only honest way there is a new active snapshot —
   * a second or two, and a catalog version. Eight prices adjusted in one
   * sitting is one import, not eight.
   */
  const applyEdits = async () => {
    if (edits.length === 0) {
      return;
    }
    setBusy(true);
    setNote({ tone: "ok", text: "Aplicando precios al catálogo…" });
    try {
      // What comes back is read from the core, so the list below is what the
      // till charges rather than what was typed.
      setProducts(await saveProducts(edits));
      setTyped({});
      setFlags({});
      setNote({
        tone: "ok",
        text: `${edits.length} producto(s) actualizado(s). El precio nuevo es el que cobra la venta.`,
      });
    } catch (reason) {
      setNote({ tone: "bad", text: describe(reason) });
    } finally {
      setBusy(false);
    }
  };

  // --- the PIN and remote control ------------------------------------------

  /** The PIN as it is being typed, before it is saved. */
  const [pinDraft, setPinDraft] = useState("");

  const savePin = async () => {
    setBusy(true);
    try {
      setLane(await setPin(pinDraft));
      setPinDraft("");
      setNote({
        tone: "ok",
        text: pinDraft.trim() === "" ? "PIN eliminado." : "PIN guardado.",
      });
    } catch (reason) {
      setNote({ tone: "bad", text: describe(reason) });
    } finally {
      setBusy(false);
    }
  };

  const toggleRemote = async (enabled: boolean) => {
    setBusy(true);
    try {
      setLane(await setRemoteControl(enabled));
      setNote(null);
    } catch (reason) {
      setNote({ tone: "bad", text: describe(reason) });
    } finally {
      setBusy(false);
    }
  };

  const edit = (patch: Partial<PrinterSettings>) => {
    setDraft((current) => (current ? { ...current, ...patch } : current));
    setNote(null);
  };

  const save = async () => {
    if (!draft) {
      return;
    }
    setBusy(true);
    try {
      const value = await savePrinterSettings(draft);
      setConfig(value);
      setDraft(value.settings);
      setNote({ tone: "ok", text: "Configuración guardada." });
    } catch (reason) {
      setNote({ tone: "bad", text: describe(reason) });
    } finally {
      setBusy(false);
    }
  };

  const probe = async () => {
    setBusy(true);
    try {
      await testPrint();
      setNote({ tone: "ok", text: "Prueba enviada a la impresora." });
    } catch (reason) {
      setNote({ tone: "bad", text: describe(reason) });
    } finally {
      setBusy(false);
      void load();
    }
  };

  /** Ask the printer itself, now. The "revisar impresora" button. */
  const askThePrinter = async () => {
    setBusy(true);
    try {
      const fresh = await printerStatus(true);
      setPaper(fresh);
      setNote(
        fresh.blocks_payment
          ? {
              tone: "bad",
              text:
                fresh.status_error ??
                "La impresora no está lista; el kiosko no cobrará.",
            }
          : { tone: "ok", text: "La impresora está lista." },
      );
    } catch (reason) {
      setNote({ tone: "bad", text: describe(reason) });
    } finally {
      setBusy(false);
    }
  };

  const wipe = async () => {
    setBusy(true);
    try {
      await resetDemo();
      onReset();
    } catch (reason) {
      setNote({ tone: "bad", text: describe(reason) });
      setBusy(false);
    }
  };

  const state = !config
    ? { label: "Cargando…", tone: "k-state--idle" }
    : !config.settings.enabled
      ? { label: "No configurada", tone: "k-state--idle" }
      : config.reachable === true
        ? { label: "Configurada · disponible", tone: "k-state--ok" }
        : config.reachable === false
          ? { label: "Configurada · no responde", tone: "k-state--bad" }
          : { label: "Configurada", tone: "k-state--idle" };

  return (
    <div className="k-admin">
      <header className="k-admin__bar">
        <Brand />
        <div className="k-header__spacer" />
        <button type="button" className="k-btn k-btn--ghost" onClick={onBack}>
          <span aria-hidden>←</span> Volver al kiosko
        </button>
      </header>

      <div className="k-admin__body">
        <div className="k-admin__head">
          <h1 className="k-h1">Configuración</h1>
          <p className="k-body">
            Ajustes de este kiosko. No se sincronizan con ningún servidor.
          </p>
        </div>

        {saleOpen ? (
          <div className="k-admin__warn">
            Hay un pedido abierto en el kiosko. Puedes revisar y guardar la
            configuración; el pedido sigue intacto y nada se cancela.
          </div>
        ) : null}

        <section className="k-panel">
          <div className="k-panel__head">
            <h2 className="k-h2">Tipo de negocio</h2>
            <span className="k-state k-state--idle">
              {lane
                ? (brandOf(lane.vertical).name)
                : "Cargando…"}
            </span>
          </div>
          <p className="k-field__hint" style={{ marginTop: 0 }}>
            Elige qué pantalla ve el cliente en este equipo. El catálogo, las
            fotografías y la impresora no cambian: sólo cambia lo que se
            muestra y lo que se puede vender aquí.
          </p>
          <div className="k-admin__lanes">
            {BRANDS.map((entry) => (
              <button
                key={entry.vertical}
                type="button"
                className={`k-admin__lane${
                  lane?.vertical === entry.vertical ? " k-admin__lane--on" : ""
                }`}
                disabled={busy || !lane}
                onClick={() => void switchLane(entry.vertical)}
              >
                <span className="k-admin__lane-name">{entry.name}</span>
                <span className="k-field__hint">{entry.detail}</span>
                {lane?.vertical === entry.vertical ? (
                  <span className="k-admin__lane-on">En uso</span>
                ) : null}
              </button>
            ))}
          </div>
          {saleOpen ? (
            <p className="k-field__hint">
              Hay un pedido abierto: al cambiar de tipo de negocio se cancela.
            </p>
          ) : null}
          {lane ? (
            <p className="k-admin__driver">
              Canal de venta: <code>{lane.channel_ref}</code>
            </p>
          ) : null}
        </section>

        <section className="k-panel" style={{ marginTop: 22 }}>
          <div className="k-panel__head">
            <h2 className="k-h2">Identidad del kiosko</h2>
            <span
              className={`k-state ${
                lane?.kiosk_open ? "k-state--ok" : "k-state--bad"
              }`}
            >
              {lane?.kiosk_open ? "Abierto" : "Cerrado"}
            </span>
          </div>
          <p className="k-field__hint" style={{ marginTop: 0 }}>
            Cómo se identifica este equipo en el piso. Texto libre: se guarda
            aquí y se publica en el estado que consulta la app de monitoreo.
          </p>
          <div className="k-admin__who">
            <label className="k-field">
              <span className="k-field__label"># Sucursal</span>
              <input
                className="k-input"
                value={who.branch}
                placeholder="001"
                onChange={(event) =>
                  setWho((current) => ({ ...current, branch: event.target.value }))
                }
              />
            </label>
            <label className="k-field">
              <span className="k-field__label"># Kiosko</span>
              <input
                className="k-input"
                value={who.kiosk}
                placeholder="K01"
                onChange={(event) =>
                  setWho((current) => ({ ...current, kiosk: event.target.value }))
                }
              />
            </label>
            <label className="k-field">
              <span className="k-field__label"># Cajero</span>
              <input
                className="k-input"
                value={who.cashier}
                placeholder="CAJERO01"
                onChange={(event) =>
                  setWho((current) => ({ ...current, cashier: event.target.value }))
                }
              />
            </label>
          </div>
          <div className="k-panel__foot">
            <button
              type="button"
              className="k-btn k-btn--ghost"
              disabled={busy || !lane}
              onClick={() =>
                void act(async () => {
                  setLane(await setKioskOpen(!(lane?.kiosk_open ?? true)));
                })
              }
            >
              {lane?.kiosk_open ? "Cerrar kiosko" : "Abrir kiosko"}
            </button>
            <button
              type="button"
              className="k-btn k-btn--primary"
              disabled={busy}
              onClick={() =>
                void act(async () => {
                  setLane(await setIdentity(who.branch, who.kiosk, who.cashier));
                }, "Identidad guardada.")
              }
            >
              Guardar
            </button>
          </div>
          {lane && !lane.kiosk_open ? (
            <p className="k-field__hint">
              La pantalla muestra «Kiosko temporalmente cerrado» y no inicia
              ventas. La aplicación sigue abierta y esta pantalla sigue
              disponible.
            </p>
          ) : null}
        </section>

        <section className="k-panel" style={{ marginTop: 22 }}>
          <div className="k-panel__head">
            <h2 className="k-h2">Día de operación</h2>
            <span
              className={`k-state ${
                day?.state === "OPEN" ? "k-state--ok" : "k-state--idle"
              }`}
            >
              {day ? (day.state === "OPEN" ? "Abierto" : "Cerrado") : "Cargando…"}
            </span>
          </div>
          {day ? (
            <dl className="k-admin__figures">
              <div>
                <dt>Sucursal</dt>
                <dd>{lane?.branch_id || "—"}</dd>
              </div>
              <div>
                <dt>Kiosko</dt>
                <dd>{lane?.kiosk_id || "—"}</dd>
              </div>
              <div>
                <dt>Cajero</dt>
                <dd>{lane?.cashier_id || "—"}</dd>
              </div>
              <div>
                <dt>Transacciones</dt>
                <dd>{day.transaction_count}</dd>
              </div>
              <div>
                <dt>Venta total</dt>
                <dd>${pesos(day.sales_total_minor)}</dd>
              </div>
              <div>
                <dt>Efectivo</dt>
                <dd>${pesos(day.cash_total_minor)}</dd>
              </div>
              <div>
                <dt>Tarjeta</dt>
                <dd>${pesos(day.card_total_minor)}</dd>
              </div>
            </dl>
          ) : null}
          {closing ? (
            <p className="k-admin__warn">
              Se cerrará el día con estas cifras. El kiosko sigue funcionando;
              para volver a vender abre un día nuevo.
            </p>
          ) : null}
          <div className="k-panel__foot">
            {day?.state === "OPEN" ? (
              closing ? (
                <>
                  <button
                    type="button"
                    className="k-btn k-btn--ghost"
                    disabled={busy}
                    onClick={() => setClosing(false)}
                  >
                    Cancelar
                  </button>
                  <button
                    type="button"
                    className="k-btn k-btn--primary"
                    disabled={busy}
                    onClick={() =>
                      void act(async () => {
                        setDay(await closeDay());
                        setClosing(false);
                      }, "Día cerrado.")
                    }
                  >
                    Confirmar cierre
                  </button>
                </>
              ) : (
                <button
                  type="button"
                  className="k-btn k-btn--ghost"
                  disabled={busy}
                  onClick={() => setClosing(true)}
                >
                  Cerrar día
                </button>
              )
            ) : (
              <button
                type="button"
                className="k-btn k-btn--primary"
                disabled={busy}
                onClick={() =>
                  void act(async () => {
                    setDay(await openDay());
                  }, "Día nuevo abierto. Contadores en cero.")
                }
              >
                Abrir nuevo día
              </button>
            )}
          </div>
        </section>

        <section className="k-panel" style={{ marginTop: 22 }}>
          <div className="k-panel__head">
            <h2 className="k-h2">CashGenic</h2>
            <span
              className={`k-state ${
                !cash?.enabled
                  ? "k-state--idle"
                  : acceptor?.online
                    ? "k-state--ok"
                    : "k-state--bad"
              }`}
            >
              {!cash?.enabled
                ? "Deshabilitado"
                : acceptor
                  ? acceptor.online
                    ? `En línea · ${acceptor.state}`
                    : "Sin conexión"
                  : "Sin probar"}
            </span>
          </div>
          <p className="k-field__hint" style={{ marginTop: 0 }}>
            Aceptador de billetes para pagos en efectivo. Deshabilitado, el
            efectivo usa el simulador de siempre.
          </p>
          {cash ? (
            <>
              <label className="k-field k-field--switch">
                <input
                  type="checkbox"
                  checked={cash.enabled}
                  disabled={busy}
                  onChange={(event) => void switchCashGenic(event.target.checked)}
                />
                <span>
                  <span className="k-field__label">Habilitar CashGenic</span>
                  <span className="k-field__hint">
                    El pago en efectivo se cobra con el dispositivo real.
                  </span>
                </span>
              </label>
              <div className="k-admin__who">
                <label className="k-field">
                  <span className="k-field__label">IP</span>
                  <input
                    className="k-input"
                    value={cash.host}
                    placeholder="192.168.1.50"
                    disabled={!cash.enabled}
                    onChange={(event) => setCash({ ...cash, host: event.target.value })}
                  />
                </label>
                <label className="k-field">
                  <span className="k-field__label">Puerto</span>
                  <input
                    className="k-input"
                    value={cash.port}
                    inputMode="numeric"
                    disabled={!cash.enabled}
                    onChange={(event) =>
                      setCash({ ...cash, port: Number(event.target.value) || 0 })
                    }
                  />
                </label>
                <label className="k-field">
                  <span className="k-field__label">Usuario API</span>
                  <input
                    className="k-input"
                    value={cash.username}
                    disabled={!cash.enabled}
                    onChange={(event) =>
                      setCash({ ...cash, username: event.target.value })
                    }
                  />
                </label>
                <label className="k-field">
                  <span className="k-field__label">Contraseña API</span>
                  <input
                    className="k-input"
                    type="password"
                    value={cash.password}
                    disabled={!cash.enabled}
                    onChange={(event) =>
                      setCash({ ...cash, password: event.target.value })
                    }
                  />
                </label>
              </div>
              <div className="k-panel__foot">
                <button
                  type="button"
                  className="k-btn k-btn--ghost"
                  disabled={busy || !cash.enabled}
                  onClick={() =>
                    void act(async () => {
                      // Saved first, so the probe tests what is stored rather
                      // than what is on screen.
                      setLane(await saveCashGenic(cash));
                      setAcceptor(await probeCashGenic());
                    })
                  }
                >
                  Probar conexión
                </button>
                <button
                  type="button"
                  className="k-btn k-btn--primary"
                  disabled={busy}
                  onClick={() =>
                    void act(async () => {
                      setLane(await saveCashGenic(cash));
                    }, "CashGenic guardado.")
                  }
                >
                  Guardar
                </button>
              </div>
              {acceptor ? (
                <dl className="k-admin__figures">
                  <div>
                    <dt>Dispositivo</dt>
                    <dd>{acceptor.device ?? "—"}</dd>
                  </div>
                  <div>
                    <dt>Estado</dt>
                    <dd>{acceptor.state}</dd>
                  </div>
                  <div>
                    <dt>Efectivo</dt>
                    <dd>
                      {acceptor.cash_total_minor === null
                        ? "—"
                        : `$${pesos(acceptor.cash_total_minor)}`}
                    </dd>
                  </div>
                </dl>
              ) : null}
              {acceptor && !acceptor.online && acceptor.detail ? (
                <p className="k-field__hint">{acceptor.detail}</p>
              ) : null}
            </>
          ) : null}
        </section>

        <section className="k-panel" style={{ marginTop: 22 }}>
          <div className="k-panel__head">
            <h2 className="k-h2">Cámara del cliente</h2>
            <span
              className={`k-state ${
                camera === "live"
                  ? "k-state--ok"
                  : camera === "denied" || camera === "unavailable"
                    ? "k-state--bad"
                    : "k-state--idle"
              }`}
            >
              {describeCamera(camera)}
            </span>
          </div>
          <label className="k-field k-field--switch">
            <input
              type="checkbox"
              checked={lane?.camera_preview ?? false}
              disabled={busy || !lane}
              onChange={(event) => void toggleCamera(event.target.checked)}
            />
            <span>
              <span className="k-field__label">
                Mostrar al cliente su propia cámara en autoservicio
              </span>
              <span className="k-field__hint">
                Sólo una vista en pantalla, en la esquina de la caja. No graba,
                no toma fotos, no guarda imágenes y no envía nada: el video no
                sale de este equipo. Apagado, no se pide permiso de cámara.
              </span>
            </span>
          </label>
          {camera === "denied" ? (
            <p className="k-field__hint">
              El sistema operativo o el navegador negó el permiso. El
              autoservicio sigue funcionando igual; concede el permiso en la
              configuración del equipo si quieres la vista previa.
            </p>
          ) : null}
          {camera === "unavailable" && lane?.camera_preview ? (
            <p className="k-field__hint">
              Este equipo no reporta ninguna cámara. El autoservicio sigue
              funcionando igual.
            </p>
          ) : null}
          {lane?.vertical === "RESTAURANT" && lane.camera_preview ? (
            <p className="k-field__hint">
              Se mostrará cuando este equipo esté en Autoservicio. El
              restaurante no tiene vista previa.
            </p>
          ) : null}
        </section>

        <section className="k-panel" style={{ marginTop: 22 }}>
          <div className="k-panel__head">
            <h2 className="k-h2">Productos y precios</h2>
            <span className="k-state k-state--idle">
              {products ? `${products.length} productos` : "Cargando…"}
            </span>
          </div>
          <p className="k-field__hint" style={{ marginTop: 0 }}>
            La lista real de la experiencia, leída del catálogo activo. El
            precio que se muestra es el que cobra la venta; al aplicar un
            cambio se reimporta el catálogo y la venta cobra el nuevo.
          </p>

          <div className="k-admin__lanes k-admin__lanes--tight">
            {BRANDS.map((entry) => (
              <button
                key={entry.vertical}
                type="button"
                className={`k-admin__lane${
                  shelf === entry.vertical ? " k-admin__lane--on" : ""
                }`}
                disabled={busy}
                onClick={() => setShelf(entry.vertical)}
              >
                <span className="k-admin__lane-name">{entry.name}</span>
                {lane?.vertical === entry.vertical ? (
                  <span className="k-admin__lane-on">En uso</span>
                ) : null}
              </button>
            ))}
          </div>

          {products === null ? (
            <p className="k-body">Leyendo el catálogo…</p>
          ) : products.length === 0 ? (
            <p className="k-admin__note k-admin__note--bad">
              Esta experiencia no tiene productos en el catálogo activo. Si
              acabas de actualizar, cierra y vuelve a abrir la aplicación.
            </p>
          ) : (
            <div className="k-admin__table" role="table">
              <div className="k-admin__row k-admin__row--head" role="row">
                <span role="columnheader">Producto</span>
                <span role="columnheader">Categoría</span>
                <span role="columnheader">Precio</span>
                {shelf === "RETAIL" ? (
                  <span role="columnheader">Autorización</span>
                ) : null}
              </div>
              {products.map((product) => {
                const draft = typed[product.product_ref];
                const shown =
                  draft !== undefined ? draft : majorOf(product.price_minor);
                const bad = draft !== undefined && draft.trim() !== "" && minorOf(draft) === null;
                return (
                  <div className="k-admin__row" role="row" key={product.product_ref}>
                    <span role="cell" className="k-admin__cell-product">
                      <AssetImage
                        asset={product.image}
                        alt={product.name}
                        className="k-admin__thumb"
                      />
                      <span>
                        <span className="k-admin__product-name">{product.name}</span>
                        <span className="k-admin__product-ref">
                          {product.sku || product.product_ref}
                        </span>
                      </span>
                    </span>
                    <span role="cell" className="k-field__hint">
                      {product.category_name || product.category_ref || "—"}
                    </span>
                    <span role="cell" className="k-admin__cell-price">
                      <input
                        className={`k-input k-input--price${bad ? " k-input--bad" : ""}`}
                        type="text"
                        inputMode="decimal"
                        aria-label={`Precio de ${product.name}`}
                        value={shown}
                        disabled={busy}
                        onChange={(event) =>
                          setTyped((current) => ({
                            ...current,
                            [product.product_ref]: event.target.value,
                          }))
                        }
                      />
                      {product.price_overridden ? (
                        <span className="k-admin__edited" title="Precio editado en este kiosko">
                          editado
                        </span>
                      ) : null}
                    </span>
                    {shelf === "RETAIL" ? (
                      <span role="cell">
                        <input
                          type="checkbox"
                          aria-label={`${product.name} requiere autorización`}
                          checked={
                            flags[product.product_ref] ??
                            product.authorization_required
                          }
                          disabled={busy}
                          onChange={(event) =>
                            setFlags((current) => ({
                              ...current,
                              [product.product_ref]: event.target.checked,
                            }))
                          }
                        />
                      </span>
                    ) : null}
                  </div>
                );
              })}
            </div>
          )}

          {shelf === "RETAIL" ? (
            <p className="k-field__hint">
              «Requiere autorización» no bloquea el escaneo. El cliente llena
              su carrito normalmente y el cobro se detiene al final, que es
              como se comporta una caja de autoservicio de verdad.
            </p>
          ) : null}

          {badPrices.length > 0 ? (
            <p className="k-admin__note k-admin__note--bad">
              {badPrices.length === 1
                ? "Hay un precio que no se entiende."
                : `Hay ${badPrices.length} precios que no se entienden.`}{" "}
              Usa pesos con hasta dos decimales, por ejemplo 59.50.
            </p>
          ) : null}

          <div className="k-panel__foot">
            <button
              type="button"
              className="k-btn k-btn--ghost"
              disabled={busy || !shelfChannel}
              onClick={() => {
                if (shelfChannel) {
                  void loadProducts(shelfChannel);
                }
              }}
            >
              Descartar cambios
            </button>
            <button
              type="button"
              className="k-btn k-btn--primary"
              disabled={busy || edits.length === 0 || badPrices.length > 0}
              onClick={() => void applyEdits()}
            >
              {edits.length === 0
                ? "Aplicar precios"
                : `Aplicar ${edits.length} cambio(s)`}
            </button>
          </div>
          <p className="k-field__hint">
            Aplicar reimporta el catálogo por el mismo camino que una
            actualización: tarda un par de segundos y sube una versión. Varios
            cambios de una vez son una sola reimportación.
          </p>
        </section>

        <section className="k-panel" style={{ marginTop: 22 }}>
          <div className="k-panel__head">
            <h2 className="k-h2">Autorización y control</h2>
            <span
              className={`k-state ${
                lane?.admin_pin ? "k-state--ok" : "k-state--idle"
              }`}
            >
              {lane?.admin_pin ? "PIN configurado" : "Sin PIN"}
            </span>
          </div>
          <p className="k-field__hint" style={{ marginTop: 0 }}>
            El PIN libera un cobro detenido por autorización y es lo único que
            protege el control remoto. Sin PIN no se autoriza nada.
          </p>

          <div className="k-field">
            <span className="k-field__label">PIN de Admin</span>
            <input
              className="k-input"
              type="password"
              inputMode="numeric"
              autoComplete="off"
              placeholder={lane?.admin_pin ? "••••" : "Al menos 4 dígitos"}
              value={pinDraft}
              disabled={busy}
              onChange={(event) => setPinDraft(event.target.value)}
            />
            <span className="k-field__hint">
              Al menos cuatro dígitos. Vacío lo elimina, y entonces no se
              puede autorizar ni controlar nada. Se guarda en claro junto a la
              base de datos, como las credenciales del aceptador: es un equipo
              de showroom que se reconfigura a mano.
            </span>
          </div>

          <label className="k-field k-field--switch">
            <input
              type="checkbox"
              checked={lane?.remote_control ?? false}
              disabled={busy || !lane?.admin_pin}
              onChange={(event) => void toggleRemote(event.target.checked)}
            />
            <span>
              <span className="k-field__label">Permitir control remoto</span>
              <span className="k-field__hint">
                Deja que una app de tienda abra o cierre este kiosko y apruebe
                autorizaciones, con este PIN y sólo desde la red local.
                Apagado por defecto: un POST que cierra el kiosko es el primer
                camino de escritura desde la red, y cuatro dígitos son poca
                protección para dejarla encendida en las oficinas de un
                cliente. <code>GET /demo/status</code> sigue abierto como
                siempre.
              </span>
            </span>
          </label>

          <div className="k-panel__foot">
            <button
              type="button"
              className="k-btn k-btn--primary"
              disabled={busy}
              onClick={() => void savePin()}
            >
              {pinDraft.trim() === "" ? "Quitar PIN" : "Guardar PIN"}
            </button>
          </div>
        </section>

        <section className="k-panel" style={{ marginTop: 22 }}>
          <div className="k-panel__head">
            <h2 className="k-h2">Impresora de recibos</h2>
            <span className={`k-state ${state.tone}`}>{state.label}</span>
          </div>

          {config && draft ? (
            <>
              <label className="k-field k-field--switch">
                <input
                  type="checkbox"
                  checked={draft.enabled}
                  onChange={(event) => edit({ enabled: event.target.checked })}
                />
                <span>
                  <span className="k-field__label">Usar impresora de recibos</span>
                  <span className="k-field__hint">
                    Apagado, este kiosko no imprime nada.
                  </span>
                </span>
              </label>

              <div className="k-field">
                <span className="k-field__label">Modelo</span>
                <select
                  className="k-select"
                  value={draft.model}
                  disabled={!draft.enabled}
                  onChange={(event) => {
                    const chosen = config.models.find(
                      (model) => model.id === event.target.value,
                    );
                    edit({
                      model: event.target.value,
                      columns: chosen ? chosen.columns : draft.columns,
                    });
                  }}
                >
                  <option value="">Selecciona un modelo…</option>
                  {config.models.map((model) => (
                    <option key={model.id} value={model.id}>
                      {model.label}
                    </option>
                  ))}
                </select>
              </div>

              <div className="k-field">
                <span className="k-field__label">Impresora</span>
                <select
                  className="k-select"
                  value={draft.queue}
                  disabled={!draft.enabled}
                  onChange={(event) => edit({ queue: event.target.value })}
                >
                  <option value="">Selecciona una impresora…</option>
                  {config.available.map((queue) => (
                    <option key={queue.name} value={queue.name}>
                      {queue.name}
                      {queue.accepting ? "" : " (detenida)"}
                    </option>
                  ))}
                </select>
                <span className="k-field__hint">
                  {config.available.length === 0
                    ? "Este equipo no reporta ninguna impresora instalada."
                    : `${config.available.length} impresora(s) detectada(s) en el sistema.`}
                </span>
              </div>

              <div className="k-field">
                <span className="k-field__label">Papel</span>
                <select
                  className="k-select"
                  value={draft.columns}
                  disabled={!draft.enabled}
                  onChange={(event) =>
                    edit({ columns: Number(event.target.value) })
                  }
                >
                  <option value={48}>80 mm · 48 caracteres</option>
                  <option value={42}>80 mm · 42 caracteres</option>
                  <option value={32}>58 mm · 32 caracteres</option>
                </select>
              </div>

              {(() => {
                // Which of the three status channels this printer is on, and
                // what each one can actually tell us. Offered as a choice
                // rather than a free-text address because "does my printer
                // have a bidirectional channel" is not a question to put to
                // whoever is setting up a stand — the host already knows
                // which USB devices are readable.
                const channel = draft.status_channel.trim();
                const kind = channel === ""
                  ? "spooler"
                  : channel.startsWith("usb")
                    ? "usb"
                    : "net";
                const devices = config.status_devices;
                const picked = kind === "usb"
                  ? (channel.startsWith("usb:") ? channel.slice(4) : "")
                  : "";
                return (
                  <>
                    <div className="k-field">
                      <span className="k-field__label">Estado de la impresora</span>
                      <select
                        className="k-select"
                        value={kind}
                        disabled={!draft.enabled}
                        onChange={(event) => {
                          const next = event.target.value;
                          edit({
                            status_channel:
                              next === "usb" ? "usb" : next === "net" ? "" : "",
                          });
                          // "Red" starts empty on purpose: an address has to
                          // be typed, and an empty one is the spooler — so
                          // the hint below says so until it is filled in.
                        }}
                      >
                        <option value="spooler">
                          Windows (cola de impresión) — sin DLE EOT
                        </option>
                        <option value="usb">USB directo — DLE EOT</option>
                        <option value="net">Red — DLE EOT</option>
                      </select>
                      <span className="k-field__hint">
                        {kind === "usb"
                          ? devices.length === 0
                            ? "Este equipo no expone ninguna impresora USB legible. Conéctala y vuelve a abrir Admin."
                            : `${devices.length} impresora(s) USB legible(s). Se le pregunta a la impresora directamente.`
                          : kind === "net"
                            ? "Se le pregunta a la impresora por su propio socket, el mismo por el que imprime."
                            : "Windows avisa de papel agotado, tapa abierta y fuera de línea, pero no de papel por acabarse. Una cola de impresión no puede devolver un byte DLE EOT."}
                      </span>
                    </div>

                    {kind === "usb" && devices.length > 1 ? (
                      <div className="k-field">
                        <span className="k-field__label">¿Cuál USB?</span>
                        <select
                          className="k-select"
                          value={picked}
                          disabled={!draft.enabled}
                          onChange={(event) =>
                            edit({
                              status_channel: event.target.value
                                ? `usb:${event.target.value}`
                                : "usb",
                            })
                          }
                        >
                          <option value="">Selecciona una…</option>
                          {devices.map((device) => (
                            <option key={device} value={device}>
                              {device}
                            </option>
                          ))}
                        </select>
                        <span className="k-field__hint">
                          Hay más de una. Con dos impresoras y ninguna elegida
                          no se adivina: se reportaría el papel de la otra.
                        </span>
                      </div>
                    ) : null}

                    {kind === "net" ? (
                      <div className="k-field">
                        <span className="k-field__label">Dirección</span>
                        <input
                          className="k-input"
                          type="text"
                          inputMode="url"
                          autoCapitalize="off"
                          autoCorrect="off"
                          spellCheck={false}
                          placeholder="192.168.88.50:9100"
                          value={channel.startsWith("usb") ? "" : channel}
                          disabled={!draft.enabled}
                          onChange={(event) =>
                            edit({ status_channel: event.target.value })
                          }
                        />
                        <span className="k-field__hint">
                          `host` o `host:puerto`. Sin puerto se usa 9100.
                        </span>
                      </div>
                    ) : null}
                  </>
                );
              })()}

              <label className="k-field k-field--switch">
                <input
                  type="checkbox"
                  checked={draft.auto_print}
                  disabled={!draft.enabled}
                  onChange={(event) => edit({ auto_print: event.target.checked })}
                />
                <span>
                  <span className="k-field__label">
                    Imprimir ticket automáticamente al finalizar
                  </span>
                  <span className="k-field__hint">
                    Si la impresión falla, la venta queda finalizada igual.
                  </span>
                </span>
              </label>

              <div className="k-panel__foot">
                <button
                  type="button"
                  className="k-btn k-btn--ghost"
                  disabled={busy || !config.settings.enabled}
                  onClick={() => void askThePrinter()}
                >
                  Revisar impresora
                </button>
                <button
                  type="button"
                  className="k-btn k-btn--ghost"
                  disabled={busy || !config.settings.enabled}
                  onClick={() => void probe()}
                >
                  Imprimir prueba
                </button>
                <button
                  type="button"
                  className="k-btn k-btn--primary"
                  disabled={busy}
                  onClick={() => void save()}
                >
                  Guardar
                </button>
              </div>

              {note ? (
                <p
                  className={`k-admin__note${
                    note.tone === "bad" ? " k-admin__note--bad" : ""
                  }`}
                >
                  {note.text}
                </p>
              ) : null}

              {(() => {
                // What the printer itself says, and what the kiosk will do
                // about it. Shown here because this is the screen somebody is
                // standing at when a lane has stopped taking money.
                const LEVELS: Record<string, string> = {
                  OK: "Papel OK",
                  NEAR_END: "Papel por acabarse",
                  END: "Sin papel",
                  UNKNOWN: "Papel desconocido",
                };
                const STATES: Record<string, string> = {
                  ONLINE: "En línea",
                  OFFLINE: "Fuera de línea",
                  ERROR: "Con error",
                  NO_RESPONSE: "No responde",
                  UNKNOWN: "Sin respuesta todavía",
                };
                const SOURCES: Record<string, string> = {
                  DLE_EOT: "la impresora (DLE EOT)",
                  SPOOLER: "Windows (cola de impresión)",
                  NONE: "nadie",
                };
                const faults = paper
                  ? [
                      paper.cover_open ? "tapa abierta" : null,
                      paper.jammed ? "atasco" : null,
                      paper.cutter_error ? "cutter" : null,
                      paper.unrecoverable_error ? "error no recuperable" : null,
                    ].filter(Boolean)
                  : [];
                return (
                  <>
                    <dl className="k-admin__figures">
                      <div>
                        <dt>Estado</dt>
                        <dd style={{ fontSize: 17 }}>
                          {paper ? (STATES[paper.state] ?? paper.state) : "—"}
                        </dd>
                      </div>
                      <div>
                        <dt>Papel</dt>
                        <dd style={{ fontSize: 17 }}>
                          {paper ? (LEVELS[paper.paper] ?? paper.paper) : "—"}
                        </dd>
                      </div>
                      <div>
                        <dt>Lo reporta</dt>
                        <dd style={{ fontSize: 17 }}>
                          {paper
                            ? (SOURCES[paper.status_source] ??
                              paper.status_source)
                            : "nadie"}
                        </dd>
                      </div>
                      <div>
                        <dt>Cobro</dt>
                        <dd style={{ fontSize: 17 }}>
                          {paper?.blocks_payment ? "BLOQUEADO" : "permitido"}
                        </dd>
                      </div>
                    </dl>
                    {faults.length > 0 ? (
                      <p className="k-admin__note k-admin__note--bad">
                        {faults.join(" · ")}
                        {paper?.status_error ? ` — ${paper.status_error}` : ""}
                      </p>
                    ) : null}
                    {paper?.blocks_payment ? (
                      <p className="k-admin__note k-admin__note--bad">
                        El kiosko no iniciará un cobro hasta que esto se
                        resuelva: nadie debe pagar por un ticket que no va a
                        salir.
                      </p>
                    ) : null}
                    {paper && paper.paper === "NEAR_END" ? (
                      <p className="k-admin__note">
                        Queda poco papel. Las ventas siguen; cambia el rollo
                        cuando puedas.
                      </p>
                    ) : null}
                    {paper && paper.status_source === "NONE" ? (
                      <p className="k-field__hint">
                        Esta impresora no puede informar de su estado, así que
                        el kiosko no bloqueará ningún cobro por ella.
                      </p>
                    ) : null}
                  </>
                );
              })()}

              {(() => {
                // What the chosen model can actually tell us. Shown because
                // the status a floor monitor sees depends on it: a printer
                // that reports nothing is honestly UNKNOWN, not OK.
                const chosen = config.models.find(
                  (model) => model.id === draft.model,
                );
                if (!chosen) {
                  return null;
                }
                const can = [
                  chosen.reports_online ? "en línea" : null,
                  chosen.reports_paper ? "nivel de papel" : null,
                  chosen.reports_jam ? "atasco / tapa abierta" : null,
                ].filter(Boolean);
                return (
                  <dl className="k-admin__figures">
                    <div>
                      <dt>Reporta</dt>
                      <dd style={{ fontSize: 17 }}>
                        {can.length > 0 ? can.join(" · ") : "nada"}
                      </dd>
                    </div>
                    <div>
                      <dt>Estados de papel</dt>
                      <dd style={{ fontSize: 17 }}>
                        {chosen.paper_states.join(" / ")}
                      </dd>
                    </div>
                  </dl>
                );
              })()}
              <p className="k-admin__driver">
                Controlador en uso: <code>{config.driver}</code>
              </p>
            </>
          ) : (
            <p className="k-body">Leyendo la configuración…</p>
          )}
        </section>

        <section className="k-panel" style={{ marginTop: 22 }}>
          <div className="k-panel__head">
            <h2 className="k-h2">Demostración</h2>
          </div>
          <p className="k-field__hint" style={{ marginTop: 0 }}>
            Abandona el pedido que esté abierto y vuelve a la pantalla de
            inicio. El menú, las fotografías y la configuración de impresora no
            se tocan.
          </p>
          <div className="k-panel__foot">
            <button
              type="button"
              className="k-btn k-btn--ghost"
              disabled={busy}
              onClick={() => void wipe()}
            >
              Restablecer demo
            </button>
          </div>
        </section>

        <PoweredBy />
      </div>
    </div>
  );
}
