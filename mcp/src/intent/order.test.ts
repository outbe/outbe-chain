import assert from "node:assert/strict";
import test from "node:test";
import { derivePhase } from "./order.js";

test("an order's phase follows the first status that decides it", () => {
  const cases: [origin: string, destination: string, expired: boolean, phase: string][] = [
    ["SETTLED", "FILLED", true, "SETTLED"],
    ["REFUNDED", "CLAIMED", true, "REFUNDED"],
    ["OPENED", "FILLED", true, "FILLED"],
    ["OPENED", "CLAIMED", true, "CLAIMED"],
    ["OPENED", "", true, "EXPIRED"],
    ["OPENED", "", false, "OPENED"],
    ["UNKNOWN", "", false, "UNKNOWN"],
  ];
  for (const [origin, destination, expired, phase] of cases) {
    assert.equal(derivePhase(origin, destination, expired).phase, phase, `${origin}/${destination}/${expired}`);
  }
  assert.equal(derivePhase("UNKNOWN", "", false).next, "-");
});
