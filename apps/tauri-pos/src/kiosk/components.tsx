/**
 * The pieces every kiosk screen is built from.
 *
 * Presentation only. The numbers these render arrive already formatted by the
 * core; nothing here decides money (ADR 002).
 */
import { Component, useEffect, useState, type ReactNode } from "react";

import { assetUrl } from "./api";
import type { AssetRef, Money } from "./types";

/* ------------------------------------------------------------- branding */

/**
 * The PX mark: two chevrons closing into a square, which reads as "PX" at a
 * glance and still works at 30px on a header.
 */
/**
 * The PX symbol, as PX Commerce draws it.
 *
 * A `P` and a crossed `X` in PX blue on the night ground the brand uses. The
 * stroke colour is the accent token rather than a literal, so a brand theme
 * that repaints the kiosk repaints the mark with it — the ground stays the
 * PX night blue either way, because that is the logo and not the theme.
 */
function PxMark() {
  return (
    <svg viewBox="0 0 48 48" fill="none" aria-hidden>
      <path
        d="M9 38V10h13.5c6 0 10 3.7 10 9.2S28.5 28.6 22.5 28.6H16"
        stroke="var(--px-blue)"
        strokeWidth="6.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M27 38 41 22M41 38 27 22"
        stroke="var(--px-blue)"
        strokeWidth="6.4"
        strokeLinecap="round"
      />
    </svg>
  );
}

export function Brand({
  large = false,
  tagline = "Self-service ordering",
}: {
  large?: boolean;
  tagline?: string;
}) {
  return (
    <div className={`k-brand${large ? " k-brand--lg" : ""}`}>
      <span className="k-brand__mark">
        <PxMark />
      </span>
      <span className="k-brand__text">
        <span className="k-brand__name">
          PX <em>Kiosk</em>
        </span>
        <span className="k-brand__tag">{tagline}</span>
      </span>
    </div>
  );
}

export function PoweredBy() {
  return <p className="k-powered">Powered by PX Commerce</p>;
}

/* ---------------------------------------------------------------- media */

/** An image from the local asset store. No network, and no remote fallback. */
export function AssetImage({
  asset,
  alt,
  className,
}: {
  asset: AssetRef | null;
  alt: string;
  className?: string;
}) {
  const [url, setUrl] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setUrl(null);
    void assetUrl(asset).then((value) => {
      if (live) {
        setUrl(value);
      }
    });
    return () => {
      live = false;
    };
  }, [asset]);

  if (!url) {
    // Absence of an image is "this product has no photo", never a broken one
    // and never a remote fallback (ADR 045).
    return <div className={className} aria-hidden />;
  }
  return <img className={className} src={url} alt={alt} />;
}

/* -------------------------------------------------------------- numbers */

export function Price({ money }: { money: Money }) {
  return <span className="k-num">${money.formatted}</span>;
}

/**
 * Format minor units the way the core formats them.
 *
 * Used **only** for the product screen's live preview, which is a convenience
 * and never an official amount: every total a customer is charged comes from
 * the core already formatted (ADR 046).
 */
export function previewAmount(minor: number): string {
  const sign = minor < 0 ? "-" : "";
  const abs = Math.abs(Math.round(minor));
  return `${sign}${Math.floor(abs / 100)}.${String(abs % 100).padStart(2, "0")}`;
}

/* ------------------------------------------------------------- controls */

export function Stepper({
  value,
  min = 1,
  max = 9,
  onChange,
}: {
  value: number;
  min?: number;
  max?: number;
  onChange: (next: number) => void;
}) {
  return (
    <div className="k-stepper">
      <button
        type="button"
        aria-label="Quitar uno"
        disabled={value <= min}
        onClick={() => onChange(value - 1)}
      >
        −
      </button>
      <span>{value}</span>
      <button
        type="button"
        aria-label="Agregar uno"
        disabled={value >= max}
        onClick={() => onChange(value + 1)}
      >
        +
      </button>
    </div>
  );
}

export function Steps({ at, of = 3 }: { at: number; of?: number }) {
  return (
    <div className="k-steps" aria-label={`Paso ${at} de ${of}`}>
      {Array.from({ length: of }, (_, index) => (
        <i key={index} className={index < at ? "on" : undefined} />
      ))}
    </div>
  );
}

/* --------------------------------------------------------------- chrome */

export function Header({
  onBack,
  backLabel,
  title,
  cartCount,
  cartTotal,
  onCart,
}: {
  onBack?: () => void;
  backLabel?: string;
  title?: string;
  cartCount: number;
  cartTotal?: Money;
  onCart?: () => void;
}) {
  return (
    <header className="k-header">
      {onBack ? (
        <button type="button" className="k-btn k-btn--ghost" onClick={onBack}>
          ← {backLabel ?? "Atrás"}
        </button>
      ) : (
        <Brand />
      )}
      {title ? <div className="k-header__title">{title}</div> : null}
      <div className="k-header__spacer" />
      {onCart && cartCount > 0 ? (
        <button type="button" className="k-cart-pill" onClick={onCart}>
          {cartTotal ? <Price money={cartTotal} /> : "Tu pedido"}
          <span className="k-cart-pill__count">{cartCount}</span>
        </button>
      ) : null}
    </header>
  );
}

export function Screen({ children }: { children: ReactNode }) {
  return <main className="k-main k-fade">{children}</main>;
}

export function Splash({ message }: { message: string }) {
  return (
    <div className="k-splash">
      <Brand large />
      <p className="k-body" style={{ marginTop: 10 }}>
        {message}
      </p>
    </div>
  );
}

/**
 * The sign on the door.
 *
 * Shown instead of the invitation when an operator has closed the kiosk. It
 * does not stop the application, the business day or Admin — closing a kiosk
 * on a showroom floor means "do not start a new order", and an operator has
 * to be able to open it again from the same machine.
 */
export function ClosedScreen({ brandName }: { brandName: string }) {
  return (
    <div className="k-closed">
      <Brand large />
      <div className="k-closed__mark" aria-hidden>
        ⏸
      </div>
      <h1 className="k-h1">Kiosko temporalmente cerrado</h1>
      <p className="k-body">
        {brandName} no está tomando pedidos en esta pantalla por el momento.
      </p>
      <p className="k-field__hint">Un miembro del equipo puede abrirlo de nuevo.</p>
      <PoweredBy />
    </div>
  );
}

/**
 * The printer has stopped the lane, and the customer is told why.
 *
 * Shown instead of letting the tender choice through. The rule it draws is
 * the core's: a cover that is open, a roll that has run out, or a printer that
 * is not answering stops a payment *before* it starts, because the one thing
 * worse than a kiosk that cannot print is a kiosk that takes the money first.
 * Paper merely near its end never gets here — that sells.
 *
 * There is no way past it but fixing the printer and pressing the button: the
 * host refuses the payment command itself, so a screen that offered a
 * "continue anyway" would only produce the same refusal one tap later.
 */
export function PrinterBlockedScreen({
  message,
  busy,
  onRetry,
}: {
  /** The core's own words, passed through rather than rewritten. */
  message: string;
  busy: boolean;
  onRetry: () => void;
}) {
  return (
    <div className="k-closed" role="alert">
      <Brand large />
      <div className="k-closed__mark" aria-hidden>
        🧾
      </div>
      {message.split("\n").map((line, at) =>
        at === 0 ? (
          <h1 className="k-h1" key={line}>
            {line}
          </h1>
        ) : (
          <p className="k-body" key={line}>
            {line}
          </p>
        ),
      )}
      <p className="k-field__hint">
        En un momento alguien se acercará a ayudarte.
      </p>
      <button
        type="button"
        className="k-btn k-btn--primary"
        disabled={busy}
        onClick={onRetry}
      >
        {busy ? "Revisando…" : "Volver a intentar"}
      </button>
      <PoweredBy />
    </div>
  );
}

/**
 * The lane is waiting for somebody to come over.
 *
 * Shown instead of letting the tender choice through when the basket holds a
 * product the shop will not let a customer pay for unsupervised. Deliberately
 * **not** shown when the product was scanned: a customer fills their basket
 * normally and finds out at the till, which is how a real self-checkout
 * behaves and is the behaviour being demonstrated.
 *
 * Two states in one screen. First the customer's: what is happening and that
 * help is coming. Then, once somebody presses Autorizar, the member of
 * staff's: a keypad for the PIN. The customer's words stay on screen while
 * the PIN is typed, because the person typing is standing beside them.
 */
export function AuthorizationScreen({
  message,
  products,
  busy,
  error,
  onApprove,
}: {
  /** The host's words, passed through rather than rewritten. */
  message: string;
  /** Which products the sale is waiting on. */
  products: { product_ref: string; name: string }[];
  busy: boolean;
  /** Why the last attempt failed, when one did. */
  error: string | null;
  onApprove: (pin: string) => void;
}) {
  const [asking, setAsking] = useState(false);
  const [pin, setPin] = useState("");

  return (
    <div className="k-closed" role="alert">
      <Brand large />
      <div className="k-closed__mark" aria-hidden>
        🔒
      </div>
      {message.split("\n").map((line, at) =>
        at === 0 ? (
          <h1 className="k-h1" key={line}>
            {line}
          </h1>
        ) : (
          <p className="k-body" key={line}>
            {line}
          </p>
        ),
      )}

      {products.length > 0 ? (
        <ul className="k-auth__list">
          {products.map((product) => (
            <li key={product.product_ref}>
              {product.name}
              <span className="k-auth__ref">{product.product_ref}</span>
            </li>
          ))}
        </ul>
      ) : null}

      {asking ? (
        <div className="k-r-pad k-auth__pad">
          <p className="k-eyebrow">PIN de autorización</p>
          <div className="k-r-pad__display">
            {pin ? "•".repeat(pin.length) : <span className="k-r-keys__ghost">••••</span>}
          </div>
          <div className="k-r-pad__keys">
            {["1", "2", "3", "4", "5", "6", "7", "8", "9"].map((digit) => (
              <button
                key={digit}
                type="button"
                className="k-r-key k-r-key--pad"
                disabled={busy}
                onClick={() => setPin((current) => (current + digit).slice(0, 12))}
              >
                {digit}
              </button>
            ))}
            <button
              type="button"
              className="k-r-key k-r-key--pad"
              aria-label="Borrar un dígito"
              disabled={busy}
              onClick={() => setPin((current) => current.slice(0, -1))}
            >
              ⌫
            </button>
            <button
              type="button"
              className="k-r-key k-r-key--pad"
              disabled={busy}
              onClick={() => setPin((current) => (current + "0").slice(0, 12))}
            >
              0
            </button>
            <button
              type="button"
              className="k-r-key k-r-key--pad k-r-key--go"
              disabled={busy || pin.length < 4}
              onClick={() => {
                onApprove(pin);
                setPin("");
              }}
            >
              ✓
            </button>
          </div>
          {error ? (
            <p className="k-admin__note k-admin__note--bad">{error}</p>
          ) : (
            <p className="k-field__hint">
              Lo teclea un miembro del equipo, no el cliente.
            </p>
          )}
        </div>
      ) : (
        <button
          type="button"
          className="k-btn k-btn--primary"
          disabled={busy}
          onClick={() => setAsking(true)}
        >
          Autorizar
        </button>
      )}
      <PoweredBy />
    </div>
  );
}

export function Toast({ message }: { message: string }) {
  return (
    <div className="k-toast" role="alert">
      <span aria-hidden>⚠</span>
      {message}
    </div>
  );
}

/* -------------------------------------------------------- failure, seen */

/**
 * A render failure shows what happened and offers a way out.
 *
 * Not a silent catch and not a blank screen: a kiosk that goes black tells
 * nobody anything, and that is precisely the failure this replaces. The
 * message is kept visible so it can be read off the machine.
 */
export class ErrorBoundary extends Component<
  { onReset: () => void; children: ReactNode },
  { error: Error | null }
> {
  constructor(props: { onReset: () => void; children: ReactNode }) {
    super(props);
    this.state = { error: null };
  }

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  override componentDidCatch(error: Error) {
    console.error("kiosk screen failed to render", error);
  }

  override render() {
    const { error } = this.state;
    if (!error) {
      return this.props.children;
    }
    return (
      <div className="k-app">
        <div className="k-crash">
          <Brand large tagline="Algo falló en pantalla" />
          <h1 className="k-h1" style={{ marginTop: 10 }}>
            No pudimos dibujar esta pantalla
          </h1>
          <p className="k-body">
            Tu pedido sigue guardado en la caja. Avisa a un miembro del equipo.
          </p>
          <code>{error.message}</code>
          <button
            type="button"
            className="k-btn k-btn--primary k-btn--lg"
            style={{ marginTop: 12 }}
            onClick={() => {
              this.setState({ error: null });
              this.props.onReset();
            }}
          >
            Volver al inicio
          </button>
        </div>
      </div>
    );
  }
}
