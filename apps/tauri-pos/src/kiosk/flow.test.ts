/**
 * The screen a pick leads to — including the one that used to be nowhere.
 *
 * The freeze these tests pin down: picking a suggestion that needed no
 * configuration added the item and left `stage` on `postadd` while the dish
 * that screen renders had already been cleared. Every render branch was false,
 * so the kiosk drew an empty window.
 *
 * Nothing here names a product. The rule is about *where a tap came from* and
 * *what came back*, which is why five of six burger suggestions hit it and one
 * did not.
 */
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { BRANDS, brandOf } from "./brands.ts";
import {
  afterAuthorizationRecheck,
  Gate,
  screenAfterPick,
  majorOf,
  minorOf,
  showsClosedSign,
  signAdopted,
  type Origin,
  type Pick,
} from "./flow.ts";
import type { PaymentGate } from "./types.ts";

const ORIGINS: Origin[] = ["menu", "cart", "postadd"];
const OUTCOMES: Pick[] = ["configure", "added", "failed"];

describe("picking a suggestion off the offer screen", () => {
  it("post_add_suggestion_simple_product: ends in the basket", () => {
    // One tap was enough, so there is nothing to configure and nothing more
    // to offer: the customer sees what they now have.
    const screen = screenAfterPick("postadd", "added");
    assert.equal(screen.stage, "cart");
    assert.equal(screen.offer, false);
  });

  it("post_add_suggestion_configurable_product: opens its own screen", () => {
    const screen = screenAfterPick("postadd", "configure");
    assert.equal(screen.stage, "product");
    assert.equal(screen.offer, false);
  });

  it("recommendation_navigation_finishes: never stays on the offer screen", () => {
    // The bug, stated as the property that forbids it. A pick that succeeded
    // always moves somewhere the kiosk can draw.
    for (const outcome of ["configure", "added"] as Pick[]) {
      const screen = screenAfterPick("postadd", outcome);
      assert.notEqual(
        screen.stage,
        "postadd",
        `a ${outcome} pick left the customer on the offer screen`,
      );
    }
  });

  it("recommendation_failure_recovers: leaves the offer screen usable", () => {
    const screen = screenAfterPick("postadd", "failed");
    assert.equal(screen.stage, "postadd");
    assert.equal(
      screen.offer,
      true,
      "a refused suggestion blanked the screen it was picked from",
    );
  });
});

describe("the offer screen is never asked to draw without its dish", () => {
  it("is the invariant that makes the freeze unreachable", () => {
    for (const from of ORIGINS) {
      for (const outcome of OUTCOMES) {
        const screen = screenAfterPick(from, outcome);
        if (screen.stage === "postadd") {
          assert.equal(
            screen.offer,
            true,
            `${from}/${outcome} returns the offer screen with no dish on it`,
          );
        } else {
          assert.equal(
            screen.offer,
            false,
            `${from}/${outcome} keeps a dish no screen will show`,
          );
        }
      }
    }
  });

  it("answers for every combination, and only with real screens", () => {
    const allowed = new Set(["menu", "cart", "product", "postadd"]);
    for (const from of ORIGINS) {
      for (const outcome of OUTCOMES) {
        const screen = screenAfterPick(from, outcome);
        assert.ok(allowed.has(screen.stage), `${from}/${outcome} -> ${screen.stage}`);
      }
    }
  });
});

describe("picking from the menu and from the basket", () => {
  it("a one-tap dish on the menu leaves the customer browsing", () => {
    assert.deepEqual(screenAfterPick("menu", "added"), {
      stage: "menu",
      offer: false,
    });
  });

  it("a dish that asks questions opens its screen from anywhere", () => {
    for (const from of ORIGINS) {
      assert.equal(screenAfterPick(from, "configure").stage, "product");
    }
  });

  it("a suggestion picked from the basket comes back to the basket", () => {
    assert.equal(screenAfterPick("cart", "added").stage, "cart");
  });

  it("a failure anywhere else goes back where it came from", () => {
    assert.equal(screenAfterPick("menu", "failed").stage, "menu");
    assert.equal(screenAfterPick("cart", "failed").stage, "cart");
  });
});

describe("recommendation_double_tap_is_safe", () => {
  it("lets one caller in and turns the second away", () => {
    const gate = new Gate();
    assert.equal(gate.enter(), true, "the first tap was refused");
    assert.equal(gate.enter(), false, "the second tap got through");
    assert.equal(gate.enter(), false);
    gate.leave();
    assert.equal(gate.enter(), true, "the gate stayed shut after the work ended");
  });

  it("counts how many of a burst of taps become work", () => {
    const gate = new Gate();
    let added = 0;
    // Ten taps inside one render, as a finger on a slow panel produces.
    for (let tap = 0; tap < 10; tap += 1) {
      if (gate.enter()) {
        added += 1;
      }
    }
    assert.equal(added, 1, "a burst of taps became more than one sale line");
    gate.leave();
    assert.equal(gate.busy, false);
  });

  it("is released even when the work it guards failed", () => {
    const gate = new Gate();
    try {
      assert.ok(gate.enter());
      throw new Error("the core refused");
    } catch {
      gate.leave();
    }
    assert.equal(gate.enter(), true, "a failed pick locked the screen out");
  });
});

describe("the closed sign", () => {
  /** Every stage the counter lanes can be on. */
  const COUNTER = [
    "loading",
    "attract",
    "fulfillment",
    "menu",
    "product",
    "postadd",
    "cart",
    "admin",
    "method",
    "payment",
    "done",
  ];
  /** And every stage the retail lane can be on. */
  const RETAIL = [
    "loading",
    "attract",
    "lane",
    "search",
    "admin",
    "method",
    "payment",
    "done",
  ];

  it("is never drawn while the kiosk is open", () => {
    for (const stage of [...COUNTER, ...RETAIL]) {
      assert.equal(
        showsClosedSign(true, stage),
        false,
        `an open kiosk showed the closed sign on \`${stage}\``,
      );
    }
  });

  it("is drawn only where a new order would begin", () => {
    for (const stage of [...COUNTER, ...RETAIL]) {
      assert.equal(
        showsClosedSign(false, stage),
        stage === "attract",
        `the closed sign on \`${stage}\` is wrong`,
      );
    }
  });

  /**
   * The requirement that is easy to break: closing the kiosk while somebody
   * is mid-order must not take the order away from them.
   */
  it("never interrupts an order already in progress", () => {
    for (const stage of ["menu", "product", "cart", "method", "payment", "done"]) {
      assert.equal(
        showsClosedSign(false, stage),
        false,
        `closing the kiosk threw a customer off \`${stage}\``,
      );
    }
    // And once they are done and the lane returns to attract, it is closed.
    assert.equal(showsClosedSign(false, "attract"), true);
  });

  /** Admin has to stay reachable: it is the only way to open it again. */
  it("is never drawn over Admin", () => {
    assert.equal(showsClosedSign(false, "admin"), false);
  });
});

describe("a price an operator types", () => {
  it("becomes minor units without passing through a float", () => {
    assert.equal(minorOf("59"), 5900);
    assert.equal(minorOf("59.5"), 5950);
    assert.equal(minorOf("59.50"), 5950);
    assert.equal(minorOf("0"), 0);
    assert.equal(minorOf("0.05"), 5);
    assert.equal(minorOf("0.01"), 1);
    assert.equal(minorOf("1234.00"), 123400);
    // Typed the way a person types money.
    assert.equal(minorOf(" 1,234.00 "), 123400);
    assert.equal(minorOf("$59.50"), 5950);
  });

  /**
   * The cases a float gets wrong. `0.1 + 0.2` and `59.99 * 100` are the
   * classic ones; this parses text, so there is nothing to get wrong.
   */
  it("keeps every cent that a float would lose", () => {
    assert.equal(minorOf("59.99"), 5999);
    assert.equal(minorOf("0.07"), 7);
    assert.equal(minorOf("8.15"), 815);
    assert.equal(minorOf("1.10"), 110);
    assert.equal(minorOf("1.1"), 110);
    // Round-trips, which is what the editable field relies on.
    for (const minor of [0, 1, 5, 99, 100, 815, 5999, 123400]) {
      assert.equal(minorOf(majorOf(minor)), minor, `${minor}`);
    }
  });

  it("refuses what is not a price rather than guessing one", () => {
    for (const typed of [
      "",
      " ",
      "abc",
      "59.555",
      "-1",
      "-0.01",
      "1e3",
      "59.",
      ".5",
      "5 9",
      "59,5",
      "1,23",
      "1,2345",
      ",500",
      "NaN",
      "Infinity",
    ]) {
      assert.equal(minorOf(typed), null, `\`${typed}\` was read as a price`);
    }
  });

  it("formats minor units the way the field shows them", () => {
    assert.equal(majorOf(5950), "59.50");
    assert.equal(majorOf(0), "0.00");
    assert.equal(majorOf(5), "0.05");
    assert.equal(majorOf(123400), "1234.00");
  });
});

// --- a hold released from somewhere other than this glass -----------------

/** A gate answer holding a basket on `requestId`. */
function held(requestId: string): PaymentGate {
  return {
    allowed: false,
    blocked_by: "AUTHORIZATION",
    message: "Este producto requiere autorización.",
    authorization: {
      required: true,
      request_id: requestId,
      sale_id: "sale-3",
      approved: false,
      requested_at: "2026-11-02T10:00:00Z",
      products: [{ product_ref: "R-ARRACHERA", name: "Arrachera" }],
    },
  };
}

/** A gate answer that lets a payment start. */
function open(): PaymentGate {
  return {
    allowed: true,
    blocked_by: "",
    message: "",
    authorization: {
      required: true,
      request_id: "req-1",
      sale_id: "sale-3",
      approved: true,
      requested_at: "2026-11-02T10:00:00Z",
      products: [],
    },
  };
}

describe("a basket held for an authorization", () => {
  it("an approval from the floor takes the hold off this screen", () => {
    // The bug: PX Console approved it, the host agreed, and the lane stayed
    // held because nothing on the glass had asked again.
    const next = afterAuthorizationRecheck(held("req-1"), open());
    assert.deepEqual(next, { kind: "released" });
  });

  it("a release does not start a payment; the customer still chooses", () => {
    // `released` carries nothing to act on but the hold coming off. There is no
    // tender, no amount and no method in it — by construction, the screen can
    // only go back to asking the customer how they want to pay.
    const next = afterAuthorizationRecheck(held("req-1"), open());
    assert.equal(Object.keys(next).length, 1);
    assert.equal(next.kind, "released");
  });

  it("a hold that is still a hold redraws nothing", () => {
    const next = afterAuthorizationRecheck(held("req-1"), held("req-1"));
    assert.deepEqual(next, { kind: "holding" });
  });

  it("a new request id means the basket grew and needs its own approval", () => {
    const fresh = held("req-2");
    const next = afterAuthorizationRecheck(held("req-1"), fresh);
    assert.deepEqual(next, { kind: "waiting", gate: fresh });
  });

  it("a printer that failed while somebody walked over wins the screen", () => {
    const broken: PaymentGate = {
      allowed: false,
      blocked_by: "PRINTER",
      message: "Impresora no disponible.",
      authorization: held("req-1").authorization,
    };
    const next = afterAuthorizationRecheck(held("req-1"), broken);
    assert.deepEqual(next, {
      kind: "printer",
      message: "Impresora no disponible.",
    });
  });

  it("a local PIN and a remote POST are the same answer here", () => {
    // Nothing in a gate answer says who approved it, so the screen cannot treat
    // the two differently even by accident.
    const afterPin = afterAuthorizationRecheck(held("req-1"), open());
    const afterPost = afterAuthorizationRecheck(held("req-1"), open());
    assert.deepEqual(afterPin, afterPost);
  });
});

// --- the sign can come down from somewhere other than this glass ----------

/** Every stage the counter lanes can be on. */
const COUNTER_STAGES = [
  "loading",
  "attract",
  "fulfillment",
  "menu",
  "product",
  "postadd",
  "cart",
  "admin",
  "method",
  "payment",
  "done",
];

/** And every stage the retail lane can be on. */
const RETAIL_STAGES = [
  "loading",
  "attract",
  "lane",
  "search",
  "admin",
  "method",
  "payment",
  "done",
];

describe("the sign applies to every lane this build ships", () => {
  it("covers all four brands and nothing is exempt", () => {
    // Stated as a property over the brand table rather than four copies of
    // one assertion: a fifth brand added tomorrow is covered the day it
    // lands, and a brand that somehow escaped the rule fails here.
    assert.equal(BRANDS.length, 4);
    for (const brand of BRANDS) {
      const stages = brandOf(brand.vertical).retail
        ? RETAIL_STAGES
        : COUNTER_STAGES;
      for (const stage of stages) {
        assert.equal(
          showsClosedSign(false, stage),
          stage === "attract",
          `${brand.vertical} on \`${stage}\` drew the wrong sign`,
        );
        assert.equal(
          showsClosedSign(true, stage),
          false,
          `${brand.vertical} showed the closed sign while open on \`${stage}\``,
        );
      }
    }
  });

  it("puts each brand on the shell its stages belong to", () => {
    // The split the test above relies on. Restaurant, Tim and Sbarro browse a
    // menu; retail scans a lane.
    assert.deepEqual(
      BRANDS.filter((brand) => !brand.retail).map((brand) => brand.vertical),
      ["RESTAURANT", "TIM_HORTONS", "SBARRO"],
    );
    assert.deepEqual(
      BRANDS.filter((brand) => brand.retail).map((brand) => brand.vertical),
      ["RETAIL"],
    );
  });

  it("closed with no order in progress is the closed screen", () => {
    // `attract` is the one stage with no sale behind it: every shell clears
    // its transaction on the way back to it.
    assert.equal(showsClosedSign(false, "attract"), true);
  });

  it("closed with an order in progress keeps the customer's flow", () => {
    for (const stage of ["menu", "product", "cart", "lane", "search", "method", "payment", "done"]) {
      assert.equal(
        showsClosedSign(false, stage),
        false,
        `closing the kiosk took a customer's order away on \`${stage}\``,
      );
    }
  });

  it("shows the sign once that order is finished and the lane is idle", () => {
    // The sequence: closed mid-payment, the customer finishes, the shell goes
    // back to attract, and only then does the lane go quiet.
    assert.equal(showsClosedSign(false, "payment"), false);
    assert.equal(showsClosedSign(false, "done"), false);
    assert.equal(showsClosedSign(false, "attract"), true);
  });

  it("never hides Admin", () => {
    // Admin is its own stage, and it is the way back to an open kiosk. A
    // closed sign drawn over it would lock an operator out of the machine.
    assert.equal(showsClosedSign(false, "admin"), false);
    assert.equal(showsClosedSign(true, "admin"), false);
  });

  it("asks nothing about the business day", () => {
    // The authority is the sign on the door. The accounting day is a
    // different state and is not an argument to this function — it cannot be
    // consulted here even by mistake.
    assert.equal(showsClosedSign.length, 2);
  });
});

describe("re-reading the host's preferences", () => {
  const lane = {
    vertical: "TIM_HORTONS",
    channel_ref: "KIOSK-TIM",
    kiosk_open: true,
    camera_preview: false,
  };

  it("takes a close that happened somewhere else", () => {
    const next = signAdopted(lane, { ...lane, kiosk_open: false });
    assert.equal(next?.kiosk_open, false);
  });

  it("takes an open that happened somewhere else", () => {
    const closed = { ...lane, kiosk_open: false };
    const next = signAdopted(closed, { ...closed, kiosk_open: true });
    assert.equal(next?.kiosk_open, true);
  });

  it("returns the very same object when the sign has not moved", () => {
    // Identity, not equality: this is what keeps a screen that is not
    // changing from re-rendering every second and a half.
    const next = signAdopted(lane, { ...lane, kiosk_open: true });
    assert.equal(next, lane);
  });

  it("never adopts the vertical, which is the mount key", () => {
    // A poll that swapped the vertical would remount the whole application
    // and discard a customer's order. Only the sign comes across.
    const anotherLane = {
      vertical: "SBARRO",
      channel_ref: "KIOSK-SBARRO",
      kiosk_open: false,
      camera_preview: true,
    };
    const next = signAdopted(lane, anotherLane);
    assert.equal(next?.kiosk_open, false);
    assert.equal(next?.vertical, "TIM_HORTONS");
    assert.equal(next?.channel_ref, "KIOSK-TIM");
    assert.equal(next?.camera_preview, false);
  });

  it("has nothing to adopt before the first read has landed", () => {
    assert.equal(signAdopted(null, { kiosk_open: false }), null);
  });
});
