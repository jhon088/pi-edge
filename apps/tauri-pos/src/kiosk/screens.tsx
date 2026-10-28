/**
 * The screens, in the order a customer walks through them.
 *
 * Each renders what the core returned and sends back intent. None computes a
 * price, a discount or an official total, and none decides that a sale was
 * paid (ADR 002, ADR 046).
 */
import { useMemo, useState } from "react";

import {
  AssetImage,
  Brand,
  PoweredBy,
  Price,
  Stepper,
  Steps,
  previewAmount,
} from "./components";
import {
  askedGroups,
  canDecrease,
  canIncrease,
  decrease,
  defaultsFor,
  groupTotal,
  hasQuantity,
  increase,
  isSingleSelect,
  quantityOf,
  requested,
  tap,
  unsatisfied,
} from "./selection";
import type {
  AssetRef,
  Menu,
  MenuItem,
  Money,
  ProductConfiguration,
  RequestedSelection,
  RetailTransaction,
  SelectionGroup,
  SelectionOption,
} from "./types";

/**
 * How the customer takes what they bought.
 *
 * `SELF_CHECKOUT` is the retail lane: there is no choice to make there — you
 * take your bag and go — but the confirmation still has to say the right
 * thing, so the lane names itself here rather than borrowing "para llevar".
 */
export type Fulfillment = "DINE_IN" | "TAKE_AWAY" | "SELF_CHECKOUT";

// ================================================================ attract

export function AttractScreen({
  heroAsset,
  onStart,
}: {
  heroAsset: AssetRef | null;
  onStart: () => void;
}) {
  return (
    <div className="k-attract" onClick={onStart} role="button" tabIndex={0}>
      <AssetImage asset={heroAsset} alt="" className="k-attract__art" />
      <div className="k-attract__scrim" />
      <div className="k-attract__inner k-on-dark">
        <Brand large />
        <div className="k-attract__body">
          <p className="k-eyebrow">Ordena · Paga · Recoge</p>
          <h1 className="k-display">
            Smash burgers
            <br />
            hechas al momento
          </h1>
          <p className="k-body" style={{ fontSize: 23, maxWidth: "82%" }}>
            Arma tu combo a tu gusto, paga aquí mismo y te avisamos cuando esté
            listo.
          </p>
          <div className="k-attract__cta">
            <span aria-hidden>👆</span> Toca para comenzar
          </div>
        </div>
        <div className="k-attract__foot">
          <div className="k-perk">
            <span aria-hidden>🔥</span> Recién hecho
          </div>
          <div className="k-perk">
            <span aria-hidden>⚡</span> Listo en minutos
          </div>
          <div className="k-perk">
            <span aria-hidden>💳</span> Pago sin contacto
          </div>
          <div style={{ flex: 1 }} />
          <PoweredBy />
        </div>
      </div>
    </div>
  );
}

// ============================================================ fulfillment

export function FulfillmentScreen({
  onPick,
}: {
  onPick: (choice: Fulfillment) => void;
}) {
  return (
    <>
      <div style={{ display: "grid", justifyItems: "center", gap: 14, padding: "18px 0 34px" }}>
        <Steps at={1} />
        <h1 className="k-h1" style={{ marginTop: 10 }}>
          ¿Dónde vas a disfrutarla?
        </h1>
        <p className="k-body">Puedes cambiarlo antes de pagar.</p>
      </div>
      <div className="k-choices">
        <button type="button" className="k-choice" onClick={() => onPick("DINE_IN")}>
          <span className="k-choice__glyph" aria-hidden>
            🍽️
          </span>
          <span className="k-h2">Comer aquí</span>
          <span className="k-body">Te lo llevamos a la mesa</span>
        </button>
        <button type="button" className="k-choice" onClick={() => onPick("TAKE_AWAY")}>
          <span className="k-choice__glyph" aria-hidden>
            🥡
          </span>
          <span className="k-h2">Para llevar</span>
          <span className="k-body">Empacado y listo para salir</span>
        </button>
      </div>
    </>
  );
}

// =================================================================== menu

function ProductCard({
  item,
  onPick,
}: {
  item: MenuItem;
  onPick: (item: MenuItem) => void;
}) {
  return (
    <button type="button" className="k-card" onClick={() => onPick(item)}>
      <div className="k-card__art">
        <AssetImage asset={item.image} alt={item.name} />
        <div className="k-card__fade" />
        {item.configurable ? (
          <span className="k-card__badge">A tu gusto</span>
        ) : null}
      </div>
      <div className="k-card__over">
        <div className="k-card__name">{item.name}</div>
        {item.description ? (
          <div className="k-card__desc">{item.description}</div>
        ) : null}
        <div className="k-card__buy">
          <span className="k-card__price">
            {item.configurable ? <span className="k-card__from">Desde</span> : null}
            <Price money={item.from_price} />
          </span>
          <span className="k-card__plus" aria-hidden>
            +
          </span>
        </div>
      </div>
    </button>
  );
}

export function MenuScreen({
  menu,
  activeCategory,
  onCategory,
  onPick,
}: {
  menu: Menu;
  activeCategory: string | null;
  onCategory: (category: string | null) => void;
  onPick: (item: MenuItem) => void;
}) {
  const hero = menu.categories[0]?.image ?? menu.items.find((i) => i.image)?.image ?? null;
  const shown = activeCategory
    ? menu.categories.filter((c) => c.category_ref === activeCategory)
    : menu.categories;
  const loose = menu.items.filter((item) => item.category_ref === null);

  // "Destacados" is the catalog's own running order, nothing more. The first
  // thing in each category is what whoever built the menu put first, which is
  // the merchandising decision already made — no scoring, no engine.
  const featured = menu.categories
    .map((category) =>
      menu.items.find(
        (item) => item.category_ref === category.category_ref && item.image !== null,
      ),
    )
    .filter((item): item is MenuItem => item !== undefined)
    .slice(0, 4);

  return (
    <>
      <section className="k-hero">
        <AssetImage asset={hero} alt="" />
        <div className="k-hero__scrim" />
        <div className="k-hero__body k-on-dark">
          <p className="k-eyebrow">El menú de hoy</p>
          <h1 className="k-h1">Arma tu combo perfecto</h1>
          {menu.promotions.length > 0 ? (
            <div className="k-promos">
              {menu.promotions.map((promotion) => (
                <span className="k-promo" key={promotion.promotion_id}>
                  <span aria-hidden>★</span>
                  {promotion.name}
                </span>
              ))}
            </div>
          ) : null}
        </div>
      </section>

      <nav className="k-rail" aria-label="Categorías">
        <button
          type="button"
          className={`k-tab k-tab--plain${activeCategory === null ? " k-tab--on" : ""}`}
          onClick={() => onCategory(null)}
        >
          Todo el menú
        </button>
        {menu.categories.map((category) => (
          <button
            type="button"
            key={category.category_ref}
            className={`k-tab${activeCategory === category.category_ref ? " k-tab--on" : ""}`}
            onClick={() => onCategory(category.category_ref)}
          >
            <AssetImage asset={category.image} alt="" />
            {category.name}
          </button>
        ))}
      </nav>

      {activeCategory === null && featured.length > 0 ? (
        <section>
          <div className="k-section">
            <h2 className="k-h2">Destacados</h2>
            <span className="k-section__count">Lo que más piden</span>
          </div>
          <div className="k-grid k-grid--feature">
            {featured.map((item) => (
              <ProductCard key={`f-${item.product_ref}`} item={item} onPick={onPick} />
            ))}
          </div>
        </section>
      ) : null}

      {shown.map((category) => {
        const items = menu.items.filter(
          (item) => item.category_ref === category.category_ref,
        );
        if (items.length === 0) {
          return null;
        }
        return (
          <section key={category.category_ref}>
            <div className="k-section">
              <h2 className="k-h2">{category.name}</h2>
              <span className="k-section__count">{items.length} opciones</span>
            </div>
            <div className="k-grid">
              {items.map((item) => (
                <ProductCard key={item.product_ref} item={item} onPick={onPick} />
              ))}
            </div>
          </section>
        );
      })}

      {activeCategory === null && loose.length > 0 ? (
        <section>
          <div className="k-section">
            <h2 className="k-h2">Más</h2>
          </div>
          <div className="k-grid">
            {loose.map((item) => (
              <ProductCard key={item.product_ref} item={item} onPick={onPick} />
            ))}
          </div>
        </section>
      ) : null}
    </>
  );
}

// ========================================================= product detail

export function ProductScreen({
  item,
  configuration,
  busy,
  onAdd,
}: {
  item: MenuItem;
  configuration: ProductConfiguration;
  busy: boolean;
  onAdd: (selections: RequestedSelection[], quantity: number) => void;
}) {
  const [chosen, setChosen] = useState<Map<string, number>>(() => {
    const defaults = new Map<string, number>();
    for (const group of configuration.groups) {
      if (group.owner_option_id !== null) {
        continue;
      }
      for (const option of group.options) {
        if (option.default_quantity > 0) {
          defaults.set(option.option_id, option.default_quantity);
        }
      }
    }
    return defaults;
  });
  const [quantity, setQuantity] = useState(1);

  const asked = askedGroups(configuration, chosen);

  // A nested question's defaults apply the moment it starts being asked.
  const withNestedDefaults = useMemo(() => {
    const next = new Map(chosen);
    let changed = false;
    for (const group of asked) {
      if (group.owner_option_id === null) {
        continue;
      }
      if (!group.options.some((option) => next.has(option.option_id))) {
        changed = defaultsFor(group, next) || changed;
      }
    }
    return changed ? next : chosen;
  }, [asked, chosen]);

  if (withNestedDefaults !== chosen) {
    setChosen(withNestedDefaults);
  }

  // Every gesture goes through `selection.ts`, which keeps the configuration
  // inside the bounds the catalog declared. This screen decides nothing about
  // what is allowed, and nothing about what anything costs.
  const onTap = (group: SelectionGroup, option: SelectionOption) =>
    setChosen((current) => tap(configuration, current, group, option));
  const onMore = (group: SelectionGroup, option: SelectionOption) =>
    setChosen((current) => increase(configuration, current, group, option));
  const onFewer = (group: SelectionGroup, option: SelectionOption) =>
    setChosen((current) => decrease(configuration, current, group, option));

  const totalOf = (group: SelectionGroup) => groupTotal(group, chosen);

  const missing = unsatisfied(configuration, chosen)[0];

  // A preview, never an official amount: every price in it came from the
  // core, and the core prices the line again the moment it is added.
  const previewMinor =
    (configuration.base_price.minor +
      asked.reduce(
        (sum, group) =>
          sum +
          group.options.reduce(
            (inner, option) => inner + option.unit_price.minor * quantityOf(chosen, option),
            0,
          ),
        0,
      )) *
    quantity;

  return (
    <div className="k-detail">
      <div className="k-detail__art">
        <AssetImage asset={item.image} alt={item.name} />
      </div>
      <div>
        <div className="k-detail__head">
          <p className="k-eyebrow">Personaliza tu pedido</p>
          <h1 className="k-h1">{configuration.name}</h1>
          {item.description ? <p className="k-body">{item.description}</p> : null}
        </div>

        {asked.map((group) => {
          const total = totalOf(group);
          const full = total >= group.max_quantity;
          const done = total >= group.min_quantity && group.min_quantity > 0;
            const visual = group.options.some((option) => option.image !== null);
            return (
            <section className="k-group" key={group.group_id}>
              <div className="k-group__head">
                <h2 className="k-h2">{group.name}</h2>
                <span
                  className={`k-pill ${
                    group.min_quantity > 0
                      ? done
                        ? "k-pill--done"
                        : "k-pill--req"
                      : "k-pill--opt"
                  }`}
                >
                  {group.min_quantity > 0
                    ? done
                      ? "Listo"
                      : group.min_quantity === group.max_quantity
                        ? `Elige ${group.min_quantity}`
                        : `Elige al menos ${group.min_quantity}`
                    : "Opcional"}
                </span>
              </div>
              <p className="k-group__rule">
                {group.min_quantity > 0 ? "Obligatorio · " : "Opcional · "}
                {group.max_quantity === 1
                  ? "elige una opción"
                  : `hasta ${group.max_quantity}`}
                {group.max_quantity > 1 ? ` · ${total}/${group.max_quantity}` : null}
              </p>
              <div className={`k-options${visual ? " k-options--visual" : ""}`}>
                {group.options.map((option) => {
                  const picked = quantityOf(chosen, option);
                  const blocked = picked === 0 && full && group.max_quantity > 1;
                  const counted = hasQuantity(group, option);
                  const body = (
                    <>
                      {option.image ? (
                        <span className="k-option__art">
                          <AssetImage asset={option.image} alt={option.name} />
                        </span>
                      ) : null}
                      <span className="k-option__box" aria-hidden>
                        {picked > 0 ? (counted ? picked : "✓") : ""}
                      </span>
                      <span className="k-option__name">{option.name}</span>
                      {option.unit_price.minor > 0 ? (
                        <span className="k-option__price">
                          +<Price money={option.unit_price} />
                        </span>
                      ) : (
                        <span className="k-option__incl">Incluido</span>
                      )}
                    </>
                  );

                  // An option you can take several of gets its own [−] n [+].
                  // Tapping the card still adds one — that is the gesture
                  // people already know — but coming back down no longer means
                  // tapping through the maximum and out the other side.
                  if (counted) {
                    const more = canIncrease(group, option, chosen);
                    const fewer = canDecrease(group, option, chosen);
                    return (
                      <div
                        key={option.option_id}
                        className={`k-option k-option--counted${
                          picked > 0 ? " k-option--on" : ""
                        }`}
                      >
                        <button
                          type="button"
                          className="k-option__tap"
                          disabled={blocked || !more}
                          aria-label={`Agregar ${option.name}`}
                          onClick={() => onMore(group, option)}
                        >
                          {body}
                        </button>
                        <div className="k-qty" role="group" aria-label={option.name}>
                          <button
                            type="button"
                            className="k-qty__btn"
                            aria-label={`Quitar un ${option.name}`}
                            disabled={!fewer}
                            onClick={() => onFewer(group, option)}
                          >
                            −
                          </button>
                          <span className="k-qty__value" aria-live="polite">
                            {picked}
                          </span>
                          <button
                            type="button"
                            className="k-qty__btn"
                            aria-label={`Agregar un ${option.name}`}
                            disabled={!more}
                            onClick={() => onMore(group, option)}
                          >
                            +
                          </button>
                        </div>
                      </div>
                    );
                  }

                  return (
                    <button
                      type="button"
                      key={option.option_id}
                      className={`k-option${picked > 0 ? " k-option--on" : ""}${
                        isSingleSelect(group) ? " k-option--round" : ""
                      }`}
                      disabled={blocked}
                      onClick={() => onTap(group, option)}
                    >
                      {body}
                    </button>
                  );
                })}
              </div>
            </section>
          );
        })}

        <div className="k-sticky">
          {missing ? (
            <p className="k-sticky__need">
              Elige {missing.min_quantity === 1 ? "1 opción" : `${missing.min_quantity} opciones`}{" "}
              en «{missing.name}» para continuar
            </p>
          ) : null}
          <div className="k-sticky__row">
            <Stepper value={quantity} onChange={setQuantity} />
            <button
              type="button"
              className="k-btn k-btn--primary k-btn--cta"
              disabled={busy || missing !== undefined}
              onClick={() =>
                onAdd(requested(chosen), quantity)
              }
            >
              <span>Agregar</span>
              <span className="k-btn__amount">${previewAmount(previewMinor)}</span>
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

// =================================================================== cart

export function CartScreen({
  transaction,
  menu,
  busy,
  onQuantity,
  onRemove,
  onKeepOrdering,
  onSuggest,
}: {
  transaction: RetailTransaction | null;
  menu: Menu;
  busy: boolean;
  onQuantity: (lineNumber: number, quantity: number) => void;
  onRemove: (lineNumber: number) => void;
  onKeepOrdering: () => void;
  onSuggest: (item: MenuItem) => void;
}) {
  const lines = transaction?.lines ?? [];

  if (lines.length === 0) {
    return (
      <div className="k-empty">
        <span className="k-empty__glyph" aria-hidden>
          🛍️
        </span>
        <h2 className="k-h1">Tu pedido está vacío</h2>
        <p className="k-body">Agrega algo del menú para continuar.</p>
        <button
          type="button"
          className="k-btn k-btn--primary k-btn--lg"
          style={{ marginTop: 8 }}
          onClick={onKeepOrdering}
        >
          Ver el menú
        </button>
      </div>
    );
  }

  const totals = transaction?.running_totals;
  const promotion = transaction?.applied_promotions[0];

  return (
    <div className="k-cart">
      {lines.map((line) => {
        const item = menu.items.find((c) => c.product_ref === line.product_ref);
        const units = Math.max(1, Math.round(line.quantity_thousandths / 1000));
        const selections = line.selections ?? [];
        return (
          <article className="k-line" key={line.line_id}>
            <div className="k-line__art">
              <AssetImage asset={item?.image ?? null} alt="" />
            </div>
            <div>
              <div className="k-line__name">{line.description}</div>
              {selections.length > 0 || line.preparation_note ? (
                <div className="k-chips">
                  {selections.map((selection) => (
                    <span
                      key={selection.option_id}
                      className={`k-chip${selection.chargeable ? " k-chip--paid" : ""}`}
                    >
                      {selection.chargeable ? "+" : "•"} {selection.name}
                      {selection.quantity > 1 ? ` ×${selection.quantity}` : ""}
                      {selection.amount.minor > 0 ? (
                        <>
                          {" · "}
                          <Price money={selection.amount} />
                        </>
                      ) : null}
                    </span>
                  ))}
                  {line.preparation_note ? (
                    <span className="k-chip k-chip--note">✎ {line.preparation_note}</span>
                  ) : null}
                </div>
              ) : null}
              <div style={{ marginTop: 16 }}>
                <Stepper
                  value={units}
                  onChange={(next) => onQuantity(line.line_number, next)}
                />
              </div>
            </div>
            <div className="k-line__right">
              <div className="k-line__total">
                <Price money={line.line_total} />
              </div>
              {line.discount_amount.minor > 0 ? (
                <div className="k-line__was">
                  <Price money={line.extended_amount} />
                </div>
              ) : null}
              <button
                type="button"
                className="k-btn k-btn--quiet"
                disabled={busy}
                onClick={() => onRemove(line.line_number)}
              >
                Quitar
              </button>
            </div>
          </article>
        );
      })}

      <Upsell
        items={suggestionsFor(menu, transaction)}
        busy={busy}
        onAdd={onSuggest}
      />

      {totals ? (
        <div className="k-totals">
          <div className="k-tr">
            <span>Subtotal</span>
            <Price money={totals.subtotal} />
          </div>
          {totals.discount_total.minor > 0 ? (
            <div className="k-tr k-tr--save">
              <span>
                <span aria-hidden>★ </span>
                {promotion ? promotion.name : "Descuentos"}
              </span>
              <span>
                −<Price money={totals.discount_total} />
              </span>
            </div>
          ) : null}
          <div className="k-tr">
            <span>IVA incluido</span>
            <Price money={totals.tax_total} />
          </div>
          <div className="k-tr k-tr--grand">
            <span>Total</span>
            <Price money={totals.grand_total} />
          </div>
        </div>
      ) : null}
    </div>
  );
}

// ================================================================ payment

export function PaymentScreen({
  transaction,
  state,
}: {
  transaction: RetailTransaction;
  state: "waiting" | "approving";
}) {
  const due =
    transaction.totals.grand_total.minor > 0
      ? transaction.totals.grand_total
      : transaction.running_totals.grand_total;
  const busy = state === "approving";

  return (
    <div className="k-pay">
      <Steps at={3} />
      <p className="k-eyebrow">Total a pagar</p>
      <div className="k-pay__amount">
        <Price money={due} />
      </div>
      <div className={`k-reader${busy ? " k-reader--busy" : ""}`}>
        {busy ? (
          <div className="k-spin" />
        ) : (
          <>
            <span className="k-reader__ring" />
            <span className="k-reader__ring" />
            <span className="k-reader__ring" />
          </>
        )}
        <div className="k-reader__card" aria-hidden>
          <span className="k-reader__chip" />
          <span className="k-reader__stripe" />
        </div>
      </div>
      <h1 className="k-h1">
        {busy ? "Procesando tu pago…" : "Acerca o inserta tu tarjeta"}
      </h1>
      <p className="k-body">
        {busy
          ? "No retires la tarjeta."
          : "Terminal de demostración · no se realiza ningún cargo real"}
      </p>
    </div>
  );
}

// =========================================================== confirmation

/**
 * Deliberately sparse: a huge order number, one line of reassurance, and a
 * countdown. Four seconds is not long enough to read anything else.
 */
export function ConfirmationScreen({
  orderCode,
  fulfillment,
  total,
  method,
  seconds,
  of,
}: {
  orderCode: string;
  fulfillment: Fulfillment;
  total: string;
  method: Method;
  seconds: number;
  of: number;
}) {
  return (
    <div className="k-done">
      <div className="k-done__tick" aria-hidden>
        ✓
      </div>
      <div className="k-order">
        <span className="k-order__label">
          {fulfillment === "SELF_CHECKOUT" ? "Ticket" : "Pedido"}
        </span>
        <span className="k-order__code">{orderCode}</span>
      </div>
      <h1 className="k-h1">
        {fulfillment === "SELF_CHECKOUT"
          ? "Gracias por tu compra"
          : "Tu pedido está en preparación"}
      </h1>
      <div className="k-done__meta">
        <span>
          Total <b>{total}</b>
        </span>
        <span>
          Pago <b>{method === "CASH" ? "Efectivo" : "Tarjeta"}</b>
        </span>
        <span>
          <b>
            {fulfillment === "DINE_IN"
              ? "Comer aquí"
              : fulfillment === "TAKE_AWAY"
                ? "Para llevar"
                : "Toma tu ticket"}
          </b>
        </span>
      </div>
      <div className="k-countdown">
        <i>
          <b style={{ width: `${Math.max(0, (seconds / of) * 100)}%` }} />
        </i>
        Volviendo al inicio…
      </div>
      <div style={{ marginTop: 10 }}>
        <PoweredBy />
      </div>
    </div>
  );
}

// ================================================= upsell, before the total

/**
 * "Completa tu pedido" — what to suggest, derived from the menu itself.
 *
 * No recommendation engine and no product references written into the screen:
 * the rule is *categories the cart does not already cover*, visited in turn so
 * the suggestions are varied, and items that can be added in one tap first.
 * The dataset decides what is in those categories, and the order it lists
 * them in decides the order they are offered.
 */
/**
 * What to offer alongside what is already in the basket.
 *
 * The list comes from the catalog — `recommended_product_refs`, declared per
 * product and travelling with the snapshot — not from a rule this screen
 * invented. The core has already dropped anything this menu cannot render, so
 * all that is left here is presentation: skip what the customer already has,
 * keep the catalog's order, and stop at four.
 */
export function recommendationsFor(
  menu: Menu,
  sources: string[],
  exclude: Iterable<string>,
): MenuItem[] {
  const byRef = new Map(menu.items.map((item) => [item.product_ref, item]));
  const skip = new Set(exclude);
  const picked: MenuItem[] = [];
  const seen = new Set<string>();

  for (const source of sources) {
    const from = byRef.get(source);
    if (!from) {
      continue;
    }
    for (const ref of from.recommended_product_refs) {
      if (skip.has(ref) || seen.has(ref) || picked.length >= 4) {
        continue;
      }
      const item = byRef.get(ref);
      if (item) {
        seen.add(ref);
        picked.push(item);
      }
    }
  }
  return picked;
}

/** What the cart suggests: everything in it recommends, minus what is in it. */
export function suggestionsFor(
  menu: Menu,
  transaction: RetailTransaction | null,
): MenuItem[] {
  const inCart = (transaction?.lines ?? []).map((line) => line.product_ref);
  if (inCart.length === 0) {
    return [];
  }
  return recommendationsFor(menu, inCart, inCart);
}

/**
 * The step between adding a dish and seeing the basket.
 *
 * Offered once, never blocking: "No, gracias" is as large as the suggestions
 * and always goes on to the cart. A dish that recommends nothing skips this
 * screen entirely rather than showing an empty one.
 */
export function PostAddScreen({
  item,
  suggestions,
  busy,
  onAdd,
  onSkip,
}: {
  item: MenuItem;
  suggestions: MenuItem[];
  busy: boolean;
  onAdd: (item: MenuItem) => void;
  onSkip: () => void;
}) {
  return (
    <div className="k-postadd">
      <div className="k-postadd__head">
        <div className="k-postadd__added">
          <span className="k-postadd__tick" aria-hidden>
            ✓
          </span>
          <span>{item.name} se agregó a tu pedido</span>
        </div>
        <h1 className="k-h1">Completa tu pedido</h1>
        <p className="k-body">Un toque y va directo a tu bandeja.</p>
      </div>

      <div className="k-postadd__grid">
        {suggestions.map((suggestion) => (
          <button
            type="button"
            className="k-offer"
            key={suggestion.product_ref}
            disabled={busy}
            onClick={() => onAdd(suggestion)}
          >
            <div className="k-offer__art">
              <AssetImage asset={suggestion.image} alt={suggestion.name} />
            </div>
            <div className="k-offer__body">
              <div className="k-offer__name">{suggestion.name}</div>
              {suggestion.description ? (
                <div className="k-offer__note">{suggestion.description}</div>
              ) : null}
              <div className="k-offer__buy">
                <span className="k-offer__price">
                  <Price money={suggestion.from_price} />
                </span>
                <span className="k-offer__add">
                  <span aria-hidden>+</span>{" "}
                  {suggestion.configurable ? "Elegir" : "Agregar"}
                </span>
              </div>
            </div>
          </button>
        ))}
      </div>

      <div className="k-postadd__foot">
        <button
          type="button"
          className="k-btn k-btn--ghost k-btn--lg"
          disabled={busy}
          onClick={onSkip}
        >
          No, gracias
        </button>
        <button
          type="button"
          className="k-btn k-btn--primary k-btn--lg"
          disabled={busy}
          onClick={onSkip}
        >
          Ver mi pedido
        </button>
      </div>
    </div>
  );
}

export function Upsell({
  items,
  busy,
  onAdd,
}: {
  items: MenuItem[];
  busy: boolean;
  onAdd: (item: MenuItem) => void;
}) {
  if (items.length === 0) {
    return null;
  }
  return (
    <section className="k-upsell">
      <div className="k-upsell__head">
        <h2 className="k-h2">Completa tu pedido</h2>
        <span className="k-section__count">Toca para agregar</span>
      </div>
      <div className="k-upsell__rail">
        {items.map((item) => (
          <button
            type="button"
            className="k-suggest"
            key={item.product_ref}
            disabled={busy}
            onClick={() => onAdd(item)}
          >
            <div className="k-suggest__art">
              <AssetImage asset={item.image} alt={item.name} />
            </div>
            <div className="k-suggest__body">
              <div className="k-suggest__name">{item.name}</div>
              <div className="k-suggest__buy">
                <span className="k-suggest__price">
                  <Price money={item.from_price} />
                </span>
                <span className="k-suggest__add">
                  <span aria-hidden>+</span> Agregar
                </span>
              </div>
            </div>
          </button>
        ))}
      </div>
    </section>
  );
}

// ======================================================== payment method

export type Method = "CASH" | "CARD";

export function MethodScreen({
  due,
  onPick,
}: {
  due: Money;
  onPick: (method: Method) => void;
}) {
  return (
    <>
      <div style={{ display: "grid", justifyItems: "center", gap: 12, padding: "10px 0 34px" }}>
        <Steps at={3} />
        <p className="k-eyebrow">Total a pagar</p>
        <div className="k-pay__amount" style={{ fontSize: "clamp(42px, 5vw, 68px)" }}>
          <Price money={due} />
        </div>
        <h1 className="k-h1" style={{ marginTop: 8 }}>
          ¿Cómo quieres pagar?
        </h1>
      </div>
      <div className="k-method">
        <button type="button" className="k-method__card" onClick={() => onPick("CASH")}>
          <span className="k-method__glyph" aria-hidden>
            💵
          </span>
          <span className="k-h2">Efectivo</span>
          <span className="k-body">Paga en el cajero del kiosko</span>
        </button>
        <button type="button" className="k-method__card" onClick={() => onPick("CARD")}>
          <span className="k-method__glyph" aria-hidden>
            💳
          </span>
          <span className="k-h2">Tarjeta</span>
          <span className="k-body">Acerca, inserta o desliza</span>
        </button>
      </div>
    </>
  );
}

// ============================================================ cash payment

export function CashScreen({
  due,
  state,
}: {
  due: Money;
  state: "waiting" | "approving";
}) {
  const busy = state === "approving";
  return (
    <div className="k-pay">
      <Steps at={3} />
      <p className="k-eyebrow">Total a pagar</p>
      <div className="k-pay__amount">
        <Price money={due} />
      </div>
      <div className={`k-cash${busy ? " k-cash--busy" : ""}`}>
        {busy ? <div className="k-spin" /> : null}
        <span className="k-cash__note" aria-hidden />
        <span className="k-cash__note" aria-hidden />
        <span className="k-cash__slot" aria-hidden />
      </div>
      <h1 className="k-h1">
        {busy ? "Contando tu efectivo…" : "Inserta tus billetes"}
      </h1>
      <p className="k-body">
        {busy
          ? "Un momento, por favor."
          : "Cajero de demostración · no se recibe dinero real"}
      </p>
    </div>
  );
}
