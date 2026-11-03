/**
 * Which way a payment goes.
 *
 * The screens and the core are the same either way; what changes is who takes
 * the cash. This is the one decision that routes it.
 */
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { cashOutcome, takesCash } from "./flow.ts";

describe("cashgenic payment routing", () => {
  it("uses the acceptor only for cash, and only when it is enabled", () => {
    assert.equal(takesCash(true, "CASH"), true);
    assert.equal(takesCash(false, "CASH"), false, "disabled must use the simulator");
  });

  it("never routes a card through the note acceptor", () => {
    assert.equal(takesCash(true, "CARD"), false);
    assert.equal(takesCash(false, "CARD"), false);
  });
});

describe("cashgenic payment completion", () => {
  it("is paid only on COMPLETE, which is PaymentComplete", () => {
    assert.equal(cashOutcome("COMPLETE"), "paid");
  });

  it("is not paid while the device is still taking or giving money", () => {
    // Covered (outstanding 0) is still PAYING until PaymentComplete.
    for (const phase of ["NONE", "PAYING", "CANCELLING", "STOPPING"]) {
      assert.equal(cashOutcome(phase), "pending", phase);
    }
  });

  it("ends without a sale when the device cancelled or stopped it", () => {
    assert.equal(cashOutcome("CANCELLED"), "ended");
    assert.equal(cashOutcome("STOPPED"), "ended");
  });
});
