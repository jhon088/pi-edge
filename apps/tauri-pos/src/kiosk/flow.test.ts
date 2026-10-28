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

import { Gate, screenAfterPick, type Origin, type Pick } from "./flow.ts";

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
