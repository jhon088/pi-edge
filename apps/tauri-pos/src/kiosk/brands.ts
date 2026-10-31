/**
 * The brands this showroom kiosk can wear.
 *
 * One application, four shells. A brand is a channel to read the menu for, a
 * name to put on the screen, and a palette — never a build, never a fork of
 * the flow, and never a second payment path (ADR 047).
 *
 * Adding one is a row here plus a dataset under `demo/<slug>/`.
 */
import type { Vertical } from "./types";

/** What the attract screen says for a brand. */
export interface Invitation {
  eyebrow: string;
  /** Two lines; the screen breaks between them. */
  headline: [string, string];
  lead: string;
  cta: string;
  /** Three short reasons to stand still. */
  perks: [string, string][];
}

/** What the attract screen says for a brand. */
export interface Invitation {
  eyebrow: string;
  /** Two lines; the screen breaks between them. */
  headline: [string, string];
  lead: string;
  cta: string;
  /** Three short reasons to stand still. */
  perks: [string, string][];
}

export interface Brand {
  vertical: Vertical;
  /** What an operator calls it in Admin. */
  name: string;
  /** One line under the name, so a stand knows which is which. */
  detail: string;
  /** The catalog channel. The host decides it; this is for display only. */
  channel: string;
  /** Stamped on the root element, so the CSS can dress the screen. */
  theme: string;
  /** Whether this lane is a self-checkout rather than a counter menu. */
  retail: boolean;
  /** What its attract screen says. Retail has its own screen and needs none. */
  invitation?: Invitation;
}

export const BRANDS: Brand[] = [
  {
    vertical: "RESTAURANT",
    name: "Restaurant Demo",
    detail: "Hamburguesas y combos. El pedido se prepara en cocina.",
    channel: "KIOSK",
    theme: "px",
    retail: false,
    invitation: {
      eyebrow: "Ordena · Paga · Recoge",
      headline: ["Smash burgers", "hechas al momento"],
      lead: "Arma tu combo a tu gusto, paga aquí mismo y te avisamos cuando esté listo.",
      cta: "Toca para comenzar",
      perks: [
        ["🔥", "Recién hecho"],
        ["⚡", "Listo en minutos"],
        ["💳", "Pago sin contacto"],
      ],
    },
  },
  {
    vertical: "TIM_HORTONS",
    name: "Tim Hortons",
    detail: "Café, donas y desayunos.",
    channel: "KIOSK-TIM",
    theme: "tim",
    retail: false,
    invitation: {
      eyebrow: "Pide · Paga · Recoge",
      headline: ["Tu café", "recién hecho"],
      lead: "Desayunos, donas y café para llevar. Pide en segundos y te avisamos cuando esté listo.",
      cta: "Toca para ordenar",
      perks: [
        ["☕", "Café del día"],
        ["🍩", "Donas frescas"],
        ["🥐", "Desayuno todo el día"],
      ],
    },
  },
  {
    vertical: "SBARRO",
    name: "Sbarro",
    detail: "Pizza de Nueva York, por rebanada o entera.",
    channel: "KIOSK-SBARRO",
    theme: "sbarro",
    retail: false,
    invitation: {
      eyebrow: "Arma · Paga · Disfruta",
      headline: ["Pizza de", "Nueva York"],
      lead: "Rebanada XL o pizza entera de 14\". Elige tus extras y pásala a recoger.",
      cta: "Toca para ordenar",
      perks: [
        ["🍕", "Rebanada XL"],
        ["🔥", "Horneada al momento"],
        ["🥗", "Ensaladas y acompañamientos"],
      ],
    },
  },
  {
    vertical: "RETAIL",
    name: "Retail",
    detail: "El cliente escanea sus productos y paga en la caja.",
    channel: "KIOSK-RETAIL",
    theme: "retail",
    retail: true,
  },
];

export function brandOf(vertical: Vertical): Brand {
  // A stored preference this build does not know is the default rather than a
  // blank screen: a showroom machine gets reconfigured by hand.
  return BRANDS.find((brand) => brand.vertical === vertical) ?? BRANDS[0]!;
}
