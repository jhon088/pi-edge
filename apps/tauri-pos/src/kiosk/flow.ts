/**
 * Where the kiosk goes when a customer picks a product.
 *
 * This existed as four lines scattered through a component, and it had a hole
 * in it: picking a *suggestion* that needed no configuration added the item
 * and then left the screen where it was — on the offer screen, whose dish had
 * already been cleared. Nothing rendered. The sale was fine in the core and
 * the customer was looking at an empty window with no way out.
 *
 * So the decision lives here, where it can be stated once and tested:
 * given where the tap came from and what came back, which screen, and does the
 * offer screen keep its dish. The rule that matters is the last one —
 * `postadd` is only ever returned together with the dish it needs, so the
 * state that caused the freeze is not reachable.
 */

import type { PaymentGate } from "./types";

/** Every screen the kiosk can be on. */
export type Stage =
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

/** Where the customer was when they picked something. */
export type Origin = "menu" | "cart" | "postadd";

/**
 * What came back from picking it.
 *
 * `configure` — the product asks questions, so its screen opens.
 * `added` — one tap was enough and the core has it.
 * `failed` — the core refused, or the request never arrived.
 */
export type Pick = "configure" | "added" | "failed";

export interface Screen {
  stage: Stage;
  /**
   * Whether the offer screen's dish must stay set.
   *
   * True exactly when `stage` is `postadd`: that screen renders nothing
   * without it.
   */
  offer: boolean;
}

export function screenAfterPick(from: Origin, outcome: Pick): Screen {
  if (outcome === "configure") {
    // The product's own screen takes over wherever the tap came from. Where
    // it goes *after* adding is `returnTo`, which the caller already set.
    return { stage: "product", offer: false };
  }

  if (outcome === "failed") {
    // Back where they were, with nothing half-done. A suggestion that could
    // not be added leaves the offer screen up — the customer can pick a
    // different one, or walk past it — rather than dropping them somewhere
    // else with a toast they may not have read.
    return from === "postadd"
      ? { stage: "postadd", offer: true }
      : { stage: from, offer: false };
  }

  switch (from) {
    // Tapping a one-tap dish on the menu adds it and leaves the customer on
    // the menu, mid-browse. That is what it has always done.
    case "menu":
      return { stage: "menu", offer: false };
    // A suggestion taken from the offer screen ends in the basket: the
    // customer is offered things once, not in a loop.
    case "postadd":
      return { stage: "cart", offer: false };
    case "cart":
      return { stage: "cart", offer: false };
  }
}

/**
 * Whether the closed sign is the screen to draw.
 *
 * Closing a kiosk on a showroom floor means **"do not start a new order"**.
 * It is a sign on the door, not a shutdown: the application keeps running,
 * Admin keeps working, the business day is untouched, and — the part that is
 * easy to get wrong — an order already in progress is never interrupted.
 *
 * So the sign is drawn at exactly one stage, the one where a new order would
 * begin. A customer who was halfway through paying when an operator closed
 * the kiosk finishes, gets their receipt, and *then* the lane goes quiet.
 *
 * It takes the stage as a string because the two shells have their own stage
 * unions — the counter lanes browse a menu and the retail lane scans a lane —
 * and the rule is the same for both. There is one place to change it, and
 * the test below pins every stage either shell can be on.
 */
export function showsClosedSign(kioskOpen: boolean, stage: string): boolean {
  return !kioskOpen && stage === "attract";
}

/**
 * What a periodic re-read of the host's preferences is allowed to change.
 *
 * **Only the sign.** An operator can flip it from Admin on this machine or
 * from a floor app over `POST /demo/control/kiosk`, and the second one has
 * nothing to tell the webview with — so the webview asks, and this decides
 * what it may take from the answer.
 *
 * Everything else in the lane is left alone, and the vertical above all: it is
 * the mount key for the whole application, so adopting a fresh copy of it from
 * a timer would throw away a customer's half-finished order the moment
 * anybody saved anything in Admin. Switching lanes is a deliberate act with
 * its own path through `onLeave`, not something a poll does by accident.
 *
 * An unchanged sign returns the **same object**, so a screen that is not
 * changing does not re-render every second and a half.
 */
export function signAdopted<T extends { kiosk_open: boolean }>(
  held: T | null,
  fresh: { kiosk_open: boolean },
): T | null {
  if (held === null || held.kiosk_open === fresh.kiosk_open) {
    return held;
  }
  return { ...held, kiosk_open: fresh.kiosk_open };
}

/**
 * Whether the note acceptor takes this payment, or the simulator does.
 *
 * Two conditions, both necessary: an operator enabled the device, and the
 * customer chose cash. A card never touches it, and a kiosk with no acceptor
 * configured keeps the behaviour it has always had.
 */
export function takesCash(cashGenic: boolean, method: "CASH" | "CARD"): boolean {
  return cashGenic && method === "CASH";
}

/**
 * One intent at a time.
 *
 * Two taps on the same card are two `add_item` calls, and the core is right to
 * honour both — it was asked twice. So the second tap has to be refused where
 * it happens. `busy` disables the buttons, but only from the next render, and
 * a double tap lands inside the same one.
 *
 * Not a timeout and not a debounce: the gate is held for exactly as long as
 * the work it guards.
 */
export class Gate {
  private held = false;

  /** Take it, if it is free. Answers whether the caller may proceed. */
  enter(): boolean {
    if (this.held) {
      return false;
    }
    this.held = true;
    return true;
  }

  leave(): void {
    this.held = false;
  }

  get busy(): boolean {
    return this.held;
  }
}

/**
 * A typed price as minor units, or `null` when it is not a price.
 *
 * Parsed as **text** and never through a float: `59.50` pesos is 5950 minor
 * units and the arithmetic to get there is two integers, because this number
 * becomes the one a sale charges and a cent lost in a float is a cent nobody
 * can account for (CLAUDE.md).
 *
 * Strict on purpose, in two places where being helpful would be worse:
 *
 * * **interior whitespace is refused.** `"5 9"` is two numbers somebody is
 *   still typing, and reading it as 59.00 would set a price they never chose.
 * * **a comma must be a well-formed thousands separator.** `"1,234.00"` is a
 *   price. `"59,5"` is ambiguous — 59.50 to half the world and 59,500
 *   half-typed to the other — so it is refused and the operator retypes it.
 *   Stripping it blindly was the first version of this, and it turned a
 *   fifty-nine peso coffee into a five-hundred-and-ninety-five peso one.
 */
export function minorOf(typed: string): number | null {
  const cleaned = typed.trim().replace(/\$/g, "");
  const plain = /^\d+(\.\d{1,2})?$/;
  const grouped = /^\d{1,3}(,\d{3})+(\.\d{1,2})?$/;
  if (!plain.test(cleaned) && !grouped.test(cleaned)) {
    return null;
  }
  const [whole, fraction = ""] = cleaned.replace(/,/g, "").split(".");
  const cents = (fraction + "00").slice(0, 2);
  return Number(whole) * 100 + Number(cents);
}

/** Minor units as the decimal an operator edits. */
export function majorOf(minor: number): string {
  const sign = minor < 0 ? "-" : "";
  const absolute = Math.abs(minor);
  return `${sign}${Math.floor(absolute / 100)}.${String(absolute % 100).padStart(2, "0")}`;
}

/**
 * What a fresh gate answer means while the lane is already holding a basket.
 *
 * The screen keeps a snapshot of *why* it stopped, and a snapshot cannot see
 * an approval that happened anywhere else. A floor app releasing the basket
 * over `POST /demo/control/authorization` changes the host's authorization
 * state and nothing tells this window — which is how a lane stayed held after
 * it had already been authorised from PX Console.
 *
 * So the lane re-asks `kiosk_payment_allowed`, the one command that owns the
 * answer, and this says what the answer means. A PIN typed on the glass and a
 * POST from the floor land on the same host state, so they are
 * indistinguishable here by construction. That is the point of it.
 */
export type Recheck =
  | { kind: "released" }
  | { kind: "holding" }
  | { kind: "waiting"; gate: PaymentGate }
  | { kind: "printer"; message: string };

export function afterAuthorizationRecheck(
  held: PaymentGate,
  fresh: PaymentGate,
): Recheck {
  if (fresh.allowed) {
    // Somebody authorised it. The hold comes off and the customer carries on
    // themselves — nothing here starts a payment on their behalf.
    return { kind: "released" };
  }
  if (fresh.blocked_by !== "AUTHORIZATION") {
    // A different problem now: the printer failed while somebody walked over.
    return { kind: "printer", message: fresh.message };
  }
  if (fresh.authorization.request_id === held.authorization.request_id) {
    // The same request, still unapproved. Nothing to redraw.
    return { kind: "holding" };
  }
  // A new request id means the basket changed underneath: another restricted
  // product went in, and the approval that was on its way does not cover it.
  // The host has already voided the old one; the screen asks for the new one.
  return { kind: "waiting", gate: fresh };
}
