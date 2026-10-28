/**
 * The payment journey, owned in one place.
 *
 * Both lanes — the restaurant kiosk and the retail self-checkout — take money
 * the same way, so they take it through the same code. There is no retail
 * payment, no retail totalizer and no retail finalize: this hook arms the two
 * waits a terminal makes a customer feel, then calls the one `pay` command,
 * and the core decides everything that matters (ADR 020, ADR 038).
 *
 * The amount is never computed here. It is the core's own grand total, handed
 * straight back to it.
 */
import { useCallback, useEffect, useRef, useState } from "react";

import { pay, printReceipt } from "./api";
import type { Method } from "./screens";
import type { RetailTransaction } from "./types";

/** What the payment screen is showing. */
export type Paying = "waiting" | "approving";

/** How long the screen waits for the tap, by tender. */
const TAP_MS: Record<Method, number> = { CASH: 2200, CARD: 2600 };

/** How long "approving" holds before the sale is actually taken. */
const APPROVE_MS = 1300;

export function usePaymentJourney({
  active,
  transaction,
  method,
  autoPrint,
  onApproved,
  onRefused,
}: {
  /** True while the payment screen is the one showing. */
  active: boolean;
  transaction: RetailTransaction | null;
  method: Method;
  /** Whether this installation was told to put a receipt out by itself. */
  autoPrint: boolean;
  onApproved: (finalized: RetailTransaction) => void;
  onRefused: (reason: unknown) => void;
}): { paying: Paying; reset: () => void } {
  const [paying, setPaying] = useState<Paying>("waiting");

  // Held in refs so a parent that re-renders cannot re-arm the timers. An
  // effect that re-ran on every render would keep pushing the charge further
  // away, and a customer would watch a screen that never resolves.
  const approved = useRef(onApproved);
  approved.current = onApproved;
  const refused = useRef(onRefused);
  refused.current = onRefused;
  const printing = useRef(autoPrint);
  printing.current = autoPrint;

  useEffect(() => {
    if (!active) {
      setPaying("waiting");
      return;
    }
    if (!transaction) {
      return;
    }
    const tap = window.setTimeout(() => setPaying("approving"), TAP_MS[method]);
    return () => window.clearTimeout(tap);
  }, [active, transaction, method]);

  useEffect(() => {
    if (!active || paying !== "approving" || !transaction) {
      return;
    }
    let live = true;
    const run = window.setTimeout(() => {
      // The tender is the one the customer chose and the amount is the one
      // the core computed. The screen never decides what is owed, and never
      // decides that it was paid.
      void pay(method, transaction.totals.grand_total.minor)
        .then((finalized) => {
          if (!live) {
            return;
          }
          approved.current(finalized);
          // Paper is a second act, after the money. The sale is finalized and
          // durable by the time this runs, so a printer that is off, jammed or
          // unplugged cannot touch it — the attempt resolves to a boolean and
          // the core records the receipt's own fate either way (ADR 043).
          if (printing.current) {
            void printReceipt(finalized.transaction_id).then((printed) => {
              if (!printed) {
                // Worth knowing at the counter, not worth alarming a customer
                // whose sale is already paid for.
                console.warn("the receipt did not print");
              }
            });
          }
        })
        .catch((reason) => {
          if (live) {
            setPaying("waiting");
            refused.current(reason);
          }
        });
    }, APPROVE_MS);
    return () => {
      live = false;
      window.clearTimeout(run);
    };
  }, [active, paying, transaction, method]);

  // Stable, so a caller can hold it in a dependency array without re-arming
  // its own timers on every render.
  const reset = useCallback(() => setPaying("waiting"), []);
  return { paying, reset };
}
