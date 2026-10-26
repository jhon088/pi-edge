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
function PxMark() {
  return (
    <svg viewBox="0 0 48 48" fill="none" aria-hidden>
      <path
        d="M9 38V10h13.5c6 0 10 3.7 10 9.2S28.5 28.6 22.5 28.6H16"
        stroke="#fff"
        strokeWidth="6.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M27 38 41 22M41 38 27 22"
        stroke="#fff"
        strokeWidth="6.4"
        strokeLinecap="round"
        opacity="0.92"
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
