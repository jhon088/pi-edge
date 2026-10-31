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

import {
  cashGenicCancel,
  cashGenicClose,
  cashGenicPay,
  cashGenicStatus,
  pay,
  printReceipt,
  recordSale,
} from "./api";
import { takesCash } from "./flow";
import type { Method } from "./screens";
import type { CashGenicStatus, RetailTransaction } from "./types";

/** What the payment screen is showing. */
export type Paying = "waiting" | "approving";

/** How long the screen waits for the tap, by tender. */
const TAP_MS: Record<Method, number> = { CASH: 2200, CARD: 2600 };

/** How long "approving" holds before the sale is actually taken. */
const APPROVE_MS = 1300;

/** How often the note acceptor is asked what it has taken. */
const CASH_POLL_MS = 800;


export function usePaymentJourney({
  active,
  transaction,
  method,
  autoPrint,
  cashGenic,
  onApproved,
  onRefused,
}: {
  /** True while the payment screen is the one showing. */
  active: boolean;
  transaction: RetailTransaction | null;
  method: Method;
  /** Whether this installation was told to put a receipt out by itself. */
  autoPrint: boolean;
  /** Whether cash is taken by a real note acceptor rather than the simulator. */
  cashGenic: boolean;
  onApproved: (finalized: RetailTransaction) => void;
  onRefused: (reason: unknown) => void;
}): {
  paying: Paying;
  /** What the note acceptor has taken, while it is the one taking it. */
  acceptor: CashGenicStatus | null;
  reset: () => void;
  cancel: () => void;
} {
  const [paying, setPaying] = useState<Paying>("waiting");
  const [acceptor, setAcceptor] = useState<CashGenicStatus | null>(null);
  /** True when the device, not the simulator, is taking this payment. */
  const byDevice = takesCash(cashGenic, method);

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
      setAcceptor(null);
      return;
    }
    if (!transaction || byDevice) {
      // A note acceptor sets its own pace: the customer is feeding paper in,
      // and nothing here should pretend to know how long that takes.
      return;
    }
    const tap = window.setTimeout(() => setPaying("approving"), TAP_MS[method]);
    return () => window.clearTimeout(tap);
  }, [active, transaction, method, byDevice]);

  // --- the note acceptor, while it is the one taking the money -------------
  //
  // Start it once, then ask it what it has. The amount it is told to take is
  // the core's own grand total; the arithmetic the customer watches comes
  // back from the host, which computes it against that same number.
  useEffect(() => {
    if (!active || !byDevice || !transaction) {
      return;
    }
    const due = transaction.totals.grand_total.minor;
    let live = true;
    let timer = 0;

    const poll = () => {
      void cashGenicStatus(due)
        .then((reported) => {
          if (!live) {
            return;
          }
          setAcceptor(reported);
          if (reported.online && reported.outstanding_minor === 0) {
            // Covered. Close the session before the sale is taken, so the
            // device is free whatever the core answers.
            setPaying("approving");
            return;
          }
          timer = window.setTimeout(poll, CASH_POLL_MS);
        })
        .catch((reason) => {
          if (live) {
            refused.current(reason);
          }
        });
    };

    void cashGenicPay(due)
      .then((started) => {
        if (!live) {
          return;
        }
        setAcceptor(started);
        timer = window.setTimeout(poll, CASH_POLL_MS);
      })
      .catch((reason) => {
        if (live) {
          refused.current(reason);
        }
      });

    return () => {
      live = false;
      window.clearTimeout(timer);
    };
  }, [active, byDevice, transaction]);

  useEffect(() => {
    if (!active || paying !== "approving" || !transaction) {
      return;
    }
    let live = true;
    const run = window.setTimeout(() => {
      // The tender is the one the customer chose and the amount is the one
      // the core computed. The screen never decides what is owed, and never
      // decides that it was paid.
      const take = byDevice
        ? // Let go of the device first: the money is in it, the session is
          // finished with, and a sale must not wait on a note acceptor.
          cashGenicClose()
            .catch(() => undefined)
            .then(() => pay(method, transaction.totals.grand_total.minor))
        : pay(method, transaction.totals.grand_total.minor);

      void take
        .then((finalized) => {
          if (!live) {
            return;
          }
          approved.current(finalized);
          // The showroom's own counter for the day. After the sale, never
          // instead of it: if this fails the sale still happened.
          void recordSale(method, finalized.totals.grand_total.minor).catch(
            () => undefined,
          );
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
    }, byDevice ? 0 : APPROVE_MS);
    return () => {
      live = false;
      window.clearTimeout(run);
    };
  }, [active, paying, transaction, method, byDevice]);

  // Stable, so a caller can hold it in a dependency array without re-arming
  // its own timers on every render.
  const reset = useCallback(() => {
    setPaying("waiting");
    setAcceptor(null);
  }, []);

  /** Give the notes back. Only meaningful while a device is taking them. */
  const cancel = useCallback(() => {
    void cashGenicCancel().catch(() => undefined);
  }, []);

  return { paying, acceptor, reset, cancel };
}
