import assert from "node:assert/strict";
import test from "node:test";
import { grindMiningNonce, miningPowHash } from "./pow.js";

// Cross-language vectors from `mining_pow_golden_vectors` in crates/core/common/src/pow.rs.
const VECTORS = [
  ["Nod", 0x1234_5678n, `0x${"11".repeat(20)}`, 42n, "0x600ce7d86d05c5587e7c77055f0942e499e7b2a4a0ae9a2252d14c31dd7750f4", 41413n],
  ["Gem", 0x1234_5678n, `0x${"11".repeat(20)}`, 42n, "0xd260e69b7940b333aa6a5beb27cf8ea3c6a760cfc6d7e5c1a1a281de65278979", 56188n],
  [
    "Nod",
    0xffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100n,
    "0xabcdef0123456789abcdef0123456789abcdef01",
    0x0102_0304_0506_0708n,
    "0x8cf7cc1ac26dd020c313013c98bb08ab8253fe31045e253e2a357ac461ac3330",
    12559n,
  ],
  ["Gem", 1n, `0x${"7a".repeat(20)}`, 0n, "0x934e259014c8059c60179c9306e8a9aa73a846cd059fca8f23b2bb898ae10fb8", 73841n],
] as const;

test("Gem and Nod mining proofs of work match the Rust golden vectors", () => {
  for (const [domain, id, owner, nonce, hash, firstValid] of VECTORS) {
    assert.equal(miningPowHash(domain, id, owner, nonce), hash);
    assert.equal(grindMiningNonce(domain, id, owner, 2), firstValid);
  }
});
