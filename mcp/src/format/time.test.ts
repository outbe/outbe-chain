import assert from "node:assert/strict";
import test from "node:test";
import { formatParam } from "../format.js";
import { epochIso as intexEpochIso } from "../intex/format.js";

test("an unset timestamp renders as null wherever it is formatted", () => {
  assert.equal(formatParam({ name: "calledAt", type: "uint64" }, 0n), null);
  assert.equal(intexEpochIso(0), null);
  assert.deepEqual(formatParam({ name: "calledAt", type: "uint64" }, 1_760_000_000n), {
    epoch: 1_760_000_000,
    iso: "2025-10-09T08:53:20.000Z",
  });
});
