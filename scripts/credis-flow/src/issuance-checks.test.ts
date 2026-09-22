import { test } from "node:test";
import assert from "node:assert/strict";
import { requireIssuanceBalance, requirePermission } from "./issuance-checks.js";

test("issuance requires pre-existing principal and live restrictive permissions", () => {
  assert.throws(() => requireIssuanceBalance(99n, 100n));
  assert.throws(() => requireIssuanceBalance(0n, 0n));
  requireIssuanceBalance(100n, 100n);
  const info = { hook: "hook", signer: "signer", policies: ["policy"] };
  requirePermission(info, "hook", "signer", "policy");
  for (const invalid of [{ ...info, hook: "revoked" }, { ...info, signer: "other" }, { ...info, policies: [] }, { ...info, policies: ["permissive"] }]) {
    assert.throws(() => requirePermission(invalid, "hook", "signer", "policy"));
  }
});
