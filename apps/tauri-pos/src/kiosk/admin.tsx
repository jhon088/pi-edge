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
  describe,
  fetchPrinterSettings,
  resetDemo,
  savePrinterSettings,
  testPrint,
} from "./api";
import { Brand, PoweredBy } from "./components";
import type { PrinterConfiguration, PrinterSettings } from "./types";

type Note = { tone: "ok" | "bad"; text: string } | null;

export function AdminScreen({
  saleOpen,
  onBack,
  onReset,
}: {
  /** A sale in progress. Admin never touches one, so it says so instead. */
  saleOpen: boolean;
  onBack: () => void;
  /** Called once the open order has been abandoned, to go back to attract. */
  onReset: () => void;
}) {
  const [config, setConfig] = useState<PrinterConfiguration | null>(null);
  const [draft, setDraft] = useState<PrinterSettings | null>(null);
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
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

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
