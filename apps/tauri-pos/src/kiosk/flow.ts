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
