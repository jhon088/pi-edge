/**
 * The rules a touch has to obey.
 *
 * Written against the *contract*, not against the demo dataset: every case
 * builds its own group and options, so nothing here knows what cheese is. If
 * one of these needed a product reference to pass, the rule would be in the
 * wrong place.
 *
 * Run by Node's own test runner, which also runs TypeScript — so the rules of
 * the one screen that holds state can be tested with no bundler, no test
 * framework and nothing added to the install.
 */
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  canDecrease,
  canIncrease,
  decrease,
  groupTotal,
  hasQuantity,
  increase,
  requested,
  tap,
  unsatisfied,
} from "./selection.ts";
import type {
  Money,
  ProductConfiguration,
  SelectionGroup,
  SelectionOption,
} from "./types.ts";

const free: Money = { minor: 0, currency: "MXN", formatted: "0.00" };

function money(minor: number): Money {
  return { minor, currency: "MXN", formatted: (minor / 100).toFixed(2) };
}

function option(
  id: string,
  max: number,
  extra: Partial<SelectionOption> = {},
): SelectionOption {
  return {
    option_id: id,
    name: id,
    component_ref: null,
    unit_price: free,
    max_quantity: max,
    default_quantity: 0,
    display_order: 0,
    image: null,
    ...extra,
  };
}

function group(
  id: string,
  min: number,
  max: number,
  options: SelectionOption[],
  extra: Partial<SelectionGroup> = {},
): SelectionGroup {
  return {
    group_id: id,
    owner_option_id: null,
    name: id,
    min_quantity: min,
    max_quantity: max,
    display_order: 0,
    options,
    ...extra,
  };
}

function product(groups: SelectionGroup[]): ProductConfiguration {
  return {
    product_ref: "ANY-PRODUCT",
    name: "Anything",
    base_price: money(10_000),
    groups,
  };
}

/** Apply a sequence of gestures, reporting the quantity after each one. */
function trail(
  configuration: ProductConfiguration,
  g: SelectionGroup,
  o: SelectionOption,
  steps: ("tap" | "+" | "-")[],
  from = new Map<string, number>(),
): number[] {
  let chosen = from;
  const seen: number[] = [];
  for (const step of steps) {
    chosen =
      step === "tap"
        ? tap(configuration, chosen, g, o)
        : step === "+"
          ? increase(configuration, chosen, g, o)
          : decrease(configuration, chosen, g, o);
    seen.push(chosen.get(o.option_id) ?? 0);
  }
  return seen;
}

describe("an option you can take several of", () => {
  const extra = option("ANY-EXTRA", 3);
  const other = option("ANY-OTHER", 3);
  const extras = group("ANY-EXTRAS", 0, 6, [extra, other]);
  const config = product([extras]);

  it("counts up one at a time", () => {
    assert.deepEqual(trail(config, extras, extra, ["+", "+"]), [1, 2]);
  });

  it("counts down one at a time", () => {
    let two = increase(config, new Map(), extras, extra);
    two = increase(config, two, extras, extra);
    assert.equal(two.get(extra.option_id), 2);
    assert.deepEqual(trail(config, extras, extra, ["-", "-"], two), [1, 0]);
  });

  it("stops at its maximum instead of starting over", () => {
    // The bug this file exists for: the fourth press used to read 0.
    assert.deepEqual(trail(config, extras, extra, ["+", "+", "+", "+", "+"]), [
      1, 2, 3, 3, 3,
    ]);
  });

  it("stops at none instead of going negative", () => {
    assert.deepEqual(trail(config, extras, extra, ["+", "-", "-", "-"]), [1, 0, 0, 0]);
  });

  it("comes all the way back down from its maximum", () => {
    assert.deepEqual(trail(config, extras, extra, ["+", "+", "+", "-", "-", "-"]), [
      1, 2, 3, 2, 1, 0,
    ]);
  });

  it("leaves the option deselected at none", () => {
    const one = increase(config, new Map(), extras, extra);
    const none = decrease(config, one, extras, extra);
    assert.equal(none.has(extra.option_id), false);
    assert.deepEqual(requested(none), []);
  });

  it("says when it can and cannot move", () => {
    const empty = new Map<string, number>();
    assert.equal(canIncrease(extras, extra, empty), true);
    assert.equal(canDecrease(extras, extra, empty), false);

    let full = empty;
    for (let press = 0; press < 3; press += 1) {
      full = increase(config, full, extras, extra);
    }
    assert.equal(canIncrease(extras, extra, full), false);
    assert.equal(canDecrease(extras, extra, full), true);
  });

  it("tapping the card is one more, and never a reset", () => {
    assert.deepEqual(trail(config, extras, extra, ["tap", "tap", "tap", "tap"]), [
      1, 2, 3, 3,
    ]);
  });

  it("does not touch its neighbours", () => {
    let chosen = increase(config, new Map(), extras, extra);
    chosen = increase(config, chosen, extras, other);
    chosen = increase(config, chosen, extras, extra);
    assert.equal(chosen.get(extra.option_id), 2);
    assert.equal(chosen.get(other.option_id), 1);
    assert.equal(groupTotal(extras, chosen), 3);
  });
});

describe("the group's own ceiling", () => {
  it("stops an option short of its own maximum when the group is full", () => {
    // Three of itself allowed, but the group accepts two selections in total.
    const extra = option("ANY-EXTRA", 3);
    const extras = group("ANY-EXTRAS", 0, 2, [extra]);
    const config = product([extras]);
    assert.deepEqual(trail(config, extras, extra, ["+", "+", "+"]), [1, 2, 2]);
  });

  it("counts every option in the group towards it", () => {
    const first = option("ANY-FIRST", 3);
    const second = option("ANY-SECOND", 3);
    const extras = group("ANY-EXTRAS", 0, 2, [first, second]);
    const config = product([extras]);
    let chosen = increase(config, new Map(), extras, first);
    chosen = increase(config, chosen, extras, second);
    assert.equal(canIncrease(extras, first, chosen), false);
    assert.equal(canIncrease(extras, second, chosen), false);
    assert.equal(groupTotal(extras, chosen), 2);
  });
});

describe("a required group", () => {
  const one = option("ANY-ONE", 3);
  const two = option("ANY-TWO", 3);
  const required = group("ANY-REQUIRED", 1, 4, [one, two]);
  const config = product([required]);

  it("will not give back its last selection", () => {
    const only = increase(config, new Map(), required, one);
    assert.equal(canDecrease(required, one, only), false);
    assert.equal(decrease(config, only, required, one).get(one.option_id), 1);
  });

  it("gives back a second one happily, and keeps the first", () => {
    let chosen = increase(config, new Map(), required, one);
    chosen = increase(config, chosen, required, one);
    assert.deepEqual(trail(config, required, one, ["-", "-"], chosen), [1, 1]);
  });

  it("lets one option go when another still satisfies it", () => {
    let chosen = increase(config, new Map(), required, one);
    chosen = increase(config, chosen, required, two);
    assert.equal(canDecrease(required, one, chosen), true);
    const fewer = decrease(config, chosen, required, one);
    assert.equal(fewer.has(one.option_id), false);
    assert.equal(fewer.get(two.option_id), 1);
  });

  it("is reported as unsatisfied until it is answered", () => {
    assert.deepEqual(
      unsatisfied(config, new Map()).map((entry) => entry.group_id),
      ["ANY-REQUIRED"],
    );
    const answered = increase(config, new Map(), required, one);
    assert.deepEqual(unsatisfied(config, answered), []);
  });
});

describe("a single-select group", () => {
  const small = option("ANY-SMALL", 1);
  const large = option("ANY-LARGE", 1);

  it("replaces the answer rather than adding to it", () => {
    const sizes = group("ANY-SIZE", 1, 1, [small, large]);
    const config = product([sizes]);
    const first = tap(config, new Map(), sizes, small);
    const second = tap(config, first, sizes, large);
    assert.deepEqual([...second.keys()], [large.option_id]);
    assert.equal(second.get(large.option_id), 1);
  });

  it("keeps its answer when it is required", () => {
    const sizes = group("ANY-SIZE", 1, 1, [small, large]);
    const config = product([sizes]);
    const chosen = tap(config, new Map(), sizes, small);
    // Tapping the chosen one again would leave a required group empty.
    assert.equal(tap(config, chosen, sizes, small).get(small.option_id), 1);
  });

  it("lets go when it is optional", () => {
    const sauces = group("ANY-SAUCE", 0, 1, [small, large]);
    const config = product([sauces]);
    const chosen = tap(config, new Map(), sauces, small);
    assert.equal(tap(config, chosen, sauces, small).size, 0);
  });

  it("gets no stepper, whatever the option says", () => {
    // Three of itself allowed, but a group that accepts one selection can
    // never hold more than one — so a stepper there would be a control
    // that lies.
    const generous = option("ANY-GENEROUS", 3);
    const sizes = group("ANY-SIZE", 1, 1, [generous]);
    assert.equal(hasQuantity(sizes, generous), false);
  });
});

describe("a plain on/off option", () => {
  const without = option("ANY-WITHOUT", 1);
  const removals = group("ANY-REMOVE", 0, 3, [without]);
  const config = product([removals]);

  it("toggles", () => {
    assert.deepEqual(trail(config, removals, without, ["tap", "tap", "tap"]), [1, 0, 1]);
  });

  it("gets no stepper", () => {
    assert.equal(hasQuantity(removals, without), false);
  });
});

describe("a question that hangs off an option", () => {
  const drink = option("ANY-DRINK", 1);
  const big = option("ANY-BIG", 1, { default_quantity: 1 });
  const sizes = group("ANY-DRINK-SIZE", 1, 1, [big], {
    owner_option_id: drink.option_id,
  });

  it("arrives with its default the moment its owner is chosen", () => {
    const drinks = group("ANY-DRINKS", 1, 1, [drink]);
    const config = product([drinks, sizes]);
    const chosen = tap(config, new Map(), drinks, drink);
    assert.equal(chosen.get(big.option_id), 1);
  });

  it("is forgotten when its owner is let go", () => {
    const optional = group("ANY-DRINKS", 0, 1, [drink]);
    const config = product([optional, sizes]);
    const chosen = tap(config, new Map(), optional, drink);
    assert.equal(chosen.get(big.option_id), 1);
    const dropped = tap(config, chosen, optional, drink);
    assert.equal(dropped.size, 0);
  });

  it("is forgotten when a counted owner comes back down to none", () => {
    const counted = option("ANY-COUNTED", 3);
    const nested = group("ANY-NESTED", 1, 1, [big], {
      owner_option_id: counted.option_id,
    });
    const extras = group("ANY-EXTRAS", 0, 4, [counted]);
    const config = product([extras, nested]);
    let chosen = increase(config, new Map(), extras, counted);
    assert.equal(chosen.get(big.option_id), 1, "the nested default did not arrive");
    chosen = increase(config, chosen, extras, counted);
    chosen = decrease(config, chosen, extras, counted);
    assert.equal(chosen.get(big.option_id), 1, "two down to one dropped the answer");
    chosen = decrease(config, chosen, extras, counted);
    assert.equal(chosen.has(counted.option_id), false);
    assert.equal(chosen.has(big.option_id), false, "the nested answer outlived its owner");
  });
});

describe("what goes on the wire", () => {
  it("is ids and quantities, and nothing else", () => {
    const extra = option("ANY-EXTRA", 3, { unit_price: money(1_500) });
    const extras = group("ANY-EXTRAS", 0, 4, [extra]);
    const config = product([extras]);
    let chosen = increase(config, new Map(), extras, extra);
    chosen = increase(config, chosen, extras, extra);
    assert.deepEqual(requested(chosen), [{ option_id: "ANY-EXTRA", quantity: 2 }]);
  });
});
