import assert from "node:assert/strict";
import { test } from "node:test";
import { intexState } from "./format.js";

test("Intex lifecycle decoder keeps the terminal numeric codes", () => {
  for (const [code, name] of ["Issued", "Called", "Expired"].entries()) {
    assert.deepEqual(intexState(code), { code, name });
    assert.deepEqual(intexState(BigInt(code)), { code, name });
  }
  assert.deepEqual(intexState(255), { code: 255, name: "unknown(255)" });
});
