/**
 * The retail lane's screens.
 *
 * A self-checkout is not a menu. There is nothing to configure, nothing to
 * recommend and no order to wait for: the customer passes products over a
 * glass, sees them land in a list, and pays. So these three screens are
 * deliberately plainer than the restaurant's — the work they do is making the
 * running total impossible to miss and the next action obvious.
 *
 * Presentation only. Every amount is the core's, rendered verbatim; nothing
 * here adds, multiplies or rounds anything (ADR 002).
 */
import { useMemo, useState } from "react";

import { AssetImage, Brand, PoweredBy, Price, Steps } from "./components";
import heroRetail from "./hero-retail.jpg";
import type { Menu, MenuItem, RetailTransaction } from "./types";

/* ================================================================ attract */

/**
 * The screen a self-checkout wears when nobody is standing at it.
 *
 * A split: the invitation on the left, a grocery still life on the right,
 * bleeding to the edge of the glass. The photograph is the point — a lane that
 * opens with a paragraph of instructions looks like equipment, and a shop
 * wants it to look like a shop.
 *
 * The hero ships with the frontend rather than coming from the catalog,
 * because it is not a product: it is the lane's own artwork, composed from
 * photographs by `scripts/retail-hero.py` and bundled into `dist`. No network,
 * no asset store round trip, nothing to resolve before the first frame.
 */
export function RetailAttractScreen({ onBrowse }: { onBrowse: () => void }) {
  return (
    <div className="k-r-attract">
      <div className="k-r-attract__copy">
        <Brand large />
        <p className="k-eyebrow k-r-attract__eyebrow">Autoservicio</p>
        <h1 className="k-r-attract__title">Escanea un producto para iniciar</h1>

        <div className="k-r-attract__scan">
          <div className="k-r-scanline" aria-hidden>
            <i />
            <i />
            <i />
            <i />
            <i />
            <i />
            <i />
          </div>
          <p className="k-r-attract__lead">Acerca el código de barras al lector</p>
        </div>

        <button
          type="button"
          className="k-btn k-btn--primary k-btn--lg k-r-attract__cta"
          onClick={onBrowse}
        >
          Buscar productos
          <span aria-hidden>→</span>
        </button>
        <p className="k-field__hint">
          ¿Producto sin código de barras? Busca el producto o teclea su código
        </p>
        <div className="k-r-attract__foot">
          <PoweredBy />
        </div>
      </div>
      <div className="k-r-attract__art">
        <img className="k-r-attract__photo" src={heroRetail} alt="" />
      </div>
    </div>
  );
}

/* =================================================================== lane */

/**
 * The lane itself: what has been scanned, and what it comes to.
 *
 * The total is the biggest thing on the screen on purpose. A customer at a
 * self-checkout is watching one number, and a till that makes them hunt for
 * it is the one complaint every supermarket gets.
 */
export function RetailCartScreen({
  transaction,
  busy,
  onQuantity,
  onRemove,
}: {
  transaction: RetailTransaction | null;
  busy: boolean;
  onQuantity: (lineNumber: number, quantity: number) => void;
  onRemove: (lineNumber: number) => void;
}) {
  const lines = transaction?.lines ?? [];
  if (lines.length === 0) {
    return (
      <div className="k-r-empty">
        <div className="k-r-scanline k-r-scanline--sm" aria-hidden>
          <i />
          <i />
          <i />
          <i />
          <i />
        </div>
        <h1 className="k-h1">Pasa tu primer producto</h1>
        <p className="k-body">
          Cada producto aparecerá aquí con su precio en cuanto lo escanees.
        </p>
      </div>
    );
  }

  return (
    <div className="k-r-lane">
      <ul className="k-r-lines">
        {lines.map((line) => {
          // The core sends the quantity both as text and in thousandths. The
          // text is what gets shown; the number is only ever used to ask for
          // one more or one fewer.
          const units = Math.max(1, Math.round(line.quantity_thousandths / 1000));
          return (
            <li className="k-r-line" key={line.line_id}>
              <div className="k-r-line__body">
                <p className="k-r-line__name">{line.description}</p>
                <p className="k-r-line__unit">
                  <Price money={line.unit_price} /> c/u
                </p>
              </div>
              <div className="k-stepper k-r-line__qty">
                <button
                  type="button"
                  aria-label="Quitar uno"
                  disabled={busy}
                  onClick={() =>
                    // One of something, minus one, is nothing: at the bottom
                    // of the range the control removes the line rather than
                    // sitting there disabled.
                    units <= 1
                      ? onRemove(line.line_number)
                      : onQuantity(line.line_number, units - 1)
                  }
                >
                  −
                </button>
                <span>{units}</span>
                <button
                  type="button"
                  aria-label="Añadir uno"
                  disabled={busy}
                  onClick={() => onQuantity(line.line_number, units + 1)}
                >
                  +
                </button>
              </div>
              <div className="k-r-line__total">
                <Price money={line.line_total} />
              </div>
              <button
                type="button"
                className="k-r-line__remove"
                aria-label={`Quitar ${line.description}`}
                disabled={busy}
                onClick={() => onRemove(line.line_number)}
              >
                ×
              </button>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/**
 * The running total, beside the lane.
 *
 * `running_totals` is the core's own figure for a sale still open, and it is
 * printed exactly as it arrived. The discount line appears only when the core
 * says there is one.
 */
export function RetailTotalPanel({
  transaction,
  busy,
  onPay,
  onBrowse,
}: {
  transaction: RetailTransaction | null;
  busy: boolean;
  onPay: () => void;
  onBrowse: () => void;
}) {
  const totals = transaction?.running_totals;
  const units = (transaction?.lines ?? []).reduce(
    (sum, line) => sum + Math.max(1, Math.round(line.quantity_thousandths / 1000)),
    0,
  );
  const empty = units === 0;

  return (
    <aside className="k-r-panel">
      <Steps at={1} of={2} />
      <p className="k-eyebrow">
        {empty
          ? "Tu bolsa está vacía"
          : `${units} ${units === 1 ? "artículo" : "artículos"}`}
      </p>
      <p className="k-r-panel__label">Total</p>
      <div className="k-r-panel__total">
        {totals ? <Price money={totals.grand_total} /> : <span className="k-num">$0.00</span>}
      </div>
      {totals && totals.discount_total.minor !== 0 ? (
        <p className="k-r-panel__saving">
          Ahorro <Price money={totals.discount_total} />
        </p>
      ) : null}
      {totals ? (
        <dl className="k-r-panel__breakdown">
          <div>
            <dt>Subtotal</dt>
            <dd>
              <Price money={totals.net_total} />
            </dd>
          </div>
          <div>
            <dt>IVA</dt>
            <dd>
              <Price money={totals.tax_total} />
            </dd>
          </div>
        </dl>
      ) : null}
      <button
        type="button"
        className="k-btn k-btn--primary k-btn--lg k-r-panel__pay"
        disabled={busy || empty}
        onClick={onPay}
      >
        Pagar
        {totals && !empty ? ` · $${totals.grand_total.formatted}` : ""}
        <span aria-hidden>→</span>
      </button>
      <button type="button" className="k-btn k-btn--ghost" onClick={onBrowse}>
        Buscar un producto
      </button>
      <p className="k-field__hint k-r-panel__hint">
        Sigue pasando productos por el lector cuando quieras.
      </p>
      <PoweredBy />
    </aside>
  );
}

/* ================================================================= search */

/**
 * Finding something without a barcode in your hand.
 *
 * Two ways in, because a lane has two kinds of problem: the picture grid for
 * "I know what it looks like" (loose fruit, something whose label will not
 * read), and the keypad for "the number is printed right here". The keypad
 * goes through the *same* scan path a reader does, so what a demo exercises
 * by hand is the real thing and not a shortcut around it.
 */
export function RetailSearchScreen({
  menu,
  busy,
  onAdd,
  onBarcode,
}: {
  menu: Menu;
  busy: boolean;
  onAdd: (item: MenuItem) => void;
  onBarcode: (barcode: string) => void;
}) {
  const [tab, setTab] = useState<"browse" | "code">("browse");
  const [category, setCategory] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [code, setCode] = useState("");

  const shown = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return menu.items.filter((item) => {
      if (category && item.category_ref !== category) {
        return false;
      }
      return needle === "" || item.name.toLowerCase().includes(needle);
    });
  }, [menu.items, category, query]);

  return (
    <div className="k-r-search">
      <div className="k-r-tabs">
        <button
          type="button"
          className={`k-r-tab${tab === "browse" ? " k-r-tab--on" : ""}`}
          onClick={() => setTab("browse")}
        >
          Buscar producto
        </button>
        <button
          type="button"
          className={`k-r-tab${tab === "code" ? " k-r-tab--on" : ""}`}
          onClick={() => setTab("code")}
        >
          Teclear código
        </button>
      </div>

      {tab === "browse" ? (
        <>
          <div className="k-r-chips">
            <button
              type="button"
              className={`k-chip${category === null ? " k-chip--on" : ""}`}
              onClick={() => setCategory(null)}
            >
              Todo
            </button>
            {menu.categories.map((entry) => (
              <button
                key={entry.category_ref}
                type="button"
                className={`k-chip${
                  category === entry.category_ref ? " k-chip--on" : ""
                }`}
                onClick={() => setCategory(entry.category_ref)}
              >
                {entry.name}
              </button>
            ))}
          </div>

          <Letters
            query={query}
            onPush={(letter) => setQuery((value) => value + letter)}
            onBack={() => setQuery((value) => value.slice(0, -1))}
            onClear={() => setQuery("")}
          />

          {shown.length === 0 ? (
            <p className="k-body k-r-search__none">
              Nada coincide con esa búsqueda. Borra una letra o elige otra
              categoría.
            </p>
          ) : (
            <div className="k-r-grid">
              {shown.map((item) => (
                <button
                  key={item.product_ref}
                  type="button"
                  className="k-r-card"
                  disabled={busy}
                  onClick={() => onAdd(item)}
                >
                  <AssetImage
                    asset={item.image}
                    alt={item.name}
                    className="k-r-card__photo"
                  />
                  <span className="k-r-card__name">{item.name}</span>
                  <span className="k-r-card__price">
                    <Price money={item.from_price} />
                  </span>
                  <span className="k-r-card__add" aria-hidden>
                    + Añadir
                  </span>
                </button>
              ))}
            </div>
          )}
        </>
      ) : (
        <Keypad
          code={code}
          busy={busy}
          onPush={(digit) => setCode((value) => (value + digit).slice(0, 18))}
          onBack={() => setCode((value) => value.slice(0, -1))}
          onSubmit={() => {
            onBarcode(code);
            setCode("");
          }}
        />
      )}
    </div>
  );
}

/** Enough of a keyboard to narrow a shelf down, with no keyboard attached. */
function Letters({
  query,
  onPush,
  onBack,
  onClear,
}: {
  query: string;
  onPush: (letter: string) => void;
  onBack: () => void;
  onClear: () => void;
}) {
  const rows = ["QWERTYUIOP", "ASDFGHJKLÑ", "ZXCVBNM"];
  return (
    <div className="k-r-keys">
      <div className="k-r-keys__field">
        <span className={query ? "" : "k-r-keys__ghost"}>
          {query || "Escribe para buscar…"}
        </span>
        {query ? (
          <button
            type="button"
            className="k-r-keys__clear"
            aria-label="Borrar la búsqueda"
            onClick={onClear}
          >
            ×
          </button>
        ) : null}
      </div>
      <div className="k-r-keys__rows">
        {rows.map((row) => (
          <div className="k-r-keys__row" key={row}>
            {[...row].map((letter) => (
              <button
                key={letter}
                type="button"
                className="k-r-key"
                onClick={() => onPush(letter.toLowerCase())}
              >
                {letter}
              </button>
            ))}
            {row === "ZXCVBNM" ? (
              <button
                type="button"
                className="k-r-key k-r-key--wide"
                aria-label="Borrar una letra"
                onClick={onBack}
              >
                ⌫
              </button>
            ) : null}
          </div>
        ))}
      </div>
    </div>
  );
}

/**
 * The barcode printed on the box, typed in.
 *
 * What leaves here is a barcode and nothing else: the core resolves it, prices
 * it and decides whether it is sellable, exactly as it does for the reader.
 */
function Keypad({
  code,
  busy,
  onPush,
  onBack,
  onSubmit,
}: {
  code: string;
  busy: boolean;
  onPush: (digit: string) => void;
  onBack: () => void;
  onSubmit: () => void;
}) {
  return (
    <div className="k-r-pad">
      <p className="k-eyebrow">Código de barras</p>
      <div className="k-r-pad__display">
        {code || <span className="k-r-keys__ghost">0000000000000</span>}
      </div>
      <div className="k-r-pad__keys">
        {["1", "2", "3", "4", "5", "6", "7", "8", "9"].map((digit) => (
          <button
            key={digit}
            type="button"
            className="k-r-key k-r-key--pad"
            onClick={() => onPush(digit)}
          >
            {digit}
          </button>
        ))}
        <button
          type="button"
          className="k-r-key k-r-key--pad"
          aria-label="Borrar un dígito"
          onClick={onBack}
        >
          ⌫
        </button>
        <button
          type="button"
          className="k-r-key k-r-key--pad"
          onClick={() => onPush("0")}
        >
          0
        </button>
        <button
          type="button"
          className="k-r-key k-r-key--pad k-r-key--go"
          disabled={busy || code.length === 0}
          onClick={onSubmit}
        >
          ✓
        </button>
      </div>
      <p className="k-field__hint">
        Los productos de la demo usan códigos que empiezan con 750.
      </p>
    </div>
  );
}
