/**
 * What a touch does to a product's configuration.
 *
 * The *rules* are the catalog's and the core's: a group says how many
 * selections it accepts and how few it will settle for, an option says how
 * many of itself may be taken, and `validate_selection_request` in
 * `edge-catalog` refuses anything outside that. None of it is invented here.
 *
 * What lives here is the one thing a screen legitimately owns: translating a
 * finger on glass into a set of choices that is **always** inside those
 * bounds. Kept apart from the rendering so the rules can be read, reasoned
 * about and tested without a kiosk — a stepper that resets at its maximum is a
 * logic bug, and logic bugs belong where tests can reach them.
 *
 * Nothing in this file prices anything. The preview the screen draws is the
 * core's own option prices multiplied out; every amount a customer is charged
 * comes back from the core (ADR 046).
 */
import type { ProductConfiguration, SelectionGroup, SelectionOption } from "./types";

/** Option id → how many of it the customer has chosen. Absent means none. */
export type Choices = Map<string, number>;

/** Which questions are in play: a product's own, plus answered nested ones. */
export function askedGroups(
  configuration: ProductConfiguration,
  chosen: Choices,
): SelectionGroup[] {
  return configuration.groups.filter(
    (group) => group.owner_option_id === null || chosen.has(group.owner_option_id),
  );
}

/** Answers to questions that hung from an option no longer chosen. */
export function dropNested(
  configuration: ProductConfiguration,
  chosen: Choices,
  optionId: string,
): void {
  for (const group of configuration.groups) {
    if (group.owner_option_id !== optionId) {
      continue;
    }
    for (const option of group.options) {
      if (chosen.delete(option.option_id)) {
        dropNested(configuration, chosen, option.option_id);
      }
    }
  }
}

/** The defaults a group arrives with, applied the moment it is asked. */
export function defaultsFor(group: SelectionGroup, into: Choices): boolean {
  let changed = false;
  for (const option of group.options) {
    if (option.default_quantity > 0) {
      into.set(option.option_id, option.default_quantity);
      changed = true;
    }
  }
  return changed;
}

export function quantityOf(chosen: Choices, option: SelectionOption): number {
  return chosen.get(option.option_id) ?? 0;
}

/** How many selections this group holds in total, across its options. */
export function groupTotal(group: SelectionGroup, chosen: Choices): number {
  return group.options.reduce((sum, option) => sum + quantityOf(chosen, option), 0);
}

/**
 * Whether this option is one you can take several of.
 *
 * Both limits have to allow it. An option that permits three of itself inside
 * a group that accepts one selection in total can still only ever be taken
 * once, so it is a choice, not a quantity — and showing it a stepper that can
 * never leave 1 would be a control that lies.
 */
export function hasQuantity(group: SelectionGroup, option: SelectionOption): boolean {
  return option.max_quantity > 1 && group.max_quantity > 1;
}

/** Single-select: picking one replaces whatever was picked before. */
export function isSingleSelect(group: SelectionGroup): boolean {
  return group.max_quantity === 1;
}

/**
 * Whether one more of this option would still be a valid configuration.
 *
 * Two ceilings, and the lower one wins: this option's own maximum, and the
 * number of selections the group has left.
 */
export function canIncrease(
  group: SelectionGroup,
  option: SelectionOption,
  chosen: Choices,
): boolean {
  return (
    quantityOf(chosen, option) < option.max_quantity &&
    groupTotal(group, chosen) < group.max_quantity
  );
}

/**
 * Whether one fewer of this option would still be a valid configuration.
 *
 * The floor is the group's minimum, not the option's: a required group that
 * holds exactly as much as it must cannot give any of it back, while the same
 * option in an optional group can go all the way to none.
 */
export function canDecrease(
  group: SelectionGroup,
  option: SelectionOption,
  chosen: Choices,
): boolean {
  if (quantityOf(chosen, option) === 0) {
    return false;
  }
  return groupTotal(group, chosen) - 1 >= group.min_quantity;
}

/**
 * One more of this option, or the configuration untouched.
 *
 * At the ceiling this is a **no-op**. It does not wrap, and it does not clear
 * the option — a control that empties itself when pressed once too often is
 * the bug this module exists to prevent.
 */
export function increase(
  configuration: ProductConfiguration,
  chosen: Choices,
  group: SelectionGroup,
  option: SelectionOption,
): Choices {
  if (!canIncrease(group, option, chosen)) {
    return chosen;
  }
  const next = new Map(chosen);
  const before = quantityOf(next, option);
  next.set(option.option_id, before + 1);
  if (before === 0) {
    // Newly chosen, so anything hanging off it starts being asked — with its
    // own defaults, exactly as it would on arrival.
    for (const nested of configuration.groups) {
      if (nested.owner_option_id === option.option_id) {
        defaultsFor(nested, next);
      }
    }
  }
  return next;
}

/** One fewer, down to none — and no further than the group allows. */
export function decrease(
  configuration: ProductConfiguration,
  chosen: Choices,
  group: SelectionGroup,
  option: SelectionOption,
): Choices {
  if (!canDecrease(group, option, chosen)) {
    return chosen;
  }
  const next = new Map(chosen);
  const before = quantityOf(next, option);
  if (before <= 1) {
    next.delete(option.option_id);
    dropNested(configuration, next, option.option_id);
  } else {
    next.set(option.option_id, before - 1);
  }
  return next;
}

/**
 * What a tap on the option itself means.
 *
 * Three shapes, decided by the contract and never by which option it is:
 *
 * * **single-select** — replace the group's answer. Tapping the one already
 *   chosen clears it only in an optional group; a required group has to hold
 *   an answer, so the tap keeps it rather than producing a state the core
 *   would refuse.
 * * **quantity** — one more, stopping at the ceiling. The stepper beside it is
 *   how you come back down.
 * * **plain** — on, or off if the group can spare it.
 */
export function tap(
  configuration: ProductConfiguration,
  chosen: Choices,
  group: SelectionGroup,
  option: SelectionOption,
): Choices {
  if (isSingleSelect(group)) {
    const wasChosen = quantityOf(chosen, option) > 0;
    const next = new Map(chosen);
    for (const sibling of group.options) {
      if (next.delete(sibling.option_id)) {
        dropNested(configuration, next, sibling.option_id);
      }
    }
    if (wasChosen && group.min_quantity === 0) {
      return next;
    }
    next.set(option.option_id, 1);
    for (const nested of configuration.groups) {
      if (nested.owner_option_id === option.option_id) {
        defaultsFor(nested, next);
      }
    }
    return next;
  }

  if (hasQuantity(group, option)) {
    return increase(configuration, chosen, group, option);
  }

  return quantityOf(chosen, option) > 0
    ? decrease(configuration, chosen, group, option)
    : increase(configuration, chosen, group, option);
}

/** The groups still short of what they require, in the order they are asked. */
export function unsatisfied(
  configuration: ProductConfiguration,
  chosen: Choices,
): SelectionGroup[] {
  return askedGroups(configuration, chosen).filter(
    (group) => groupTotal(group, chosen) < group.min_quantity,
  );
}

/** What goes on the wire: ids and quantities, never a price. */
export function requested(chosen: Choices): { option_id: string; quantity: number }[] {
  return [...chosen.entries()].map(([option_id, quantity]) => ({ option_id, quantity }));
}
