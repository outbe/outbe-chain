import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import type { Address, Hex } from "viem";
import { GEM_MINING_TAG, NOD_MINING_TAG, POW_DIFFICULTY, grindMiningNonce, miningPowHash } from "./pow.js";

// Computed outside both implementations; crates/core/common/src/pow.rs reads the same file.
const fixture = JSON.parse(readFileSync(new URL("./pow.vectors.json", import.meta.url), "utf8")) as {
  difficulty_bytes: number;
  vectors: {
    domain: "nod" | "gem";
    domain_tag: string;
    right_id: string;
    owner: Address;
    mining_sequence: number;
    nonce: string;
    hash: Hex;
    first_valid_nonce: string;
    first_valid_hash: Hex;
  }[];
};

test("the miner's difficulty and domain tags are the node's", () => {
  assert.equal(POW_DIFFICULTY, fixture.difficulty_bytes);
  for (const vector of fixture.vectors) {
    assert.equal(vector.domain_tag, vector.domain === "nod" ? NOD_MINING_TAG : GEM_MINING_TAG);
  }
});

test("the miner reproduces every shared Nod and Gem mining digest and first valid nonce", () => {
  assert.ok(fixture.vectors.length > 0);
  for (const vector of fixture.vectors) {
    const rightId = BigInt(vector.right_id);
    const sequence = BigInt(vector.mining_sequence);
    assert.equal(miningPowHash(vector.domain_tag, rightId, vector.owner, sequence, BigInt(vector.nonce)), vector.hash);
    const firstValid = grindMiningNonce(vector.domain_tag, rightId, vector.owner);
    assert.equal(firstValid, BigInt(vector.first_valid_nonce));
    assert.equal(miningPowHash(vector.domain_tag, rightId, vector.owner, sequence, firstValid), vector.first_valid_hash);
  }
});

test("malformed byte fields are rejected instead of silently hashed", () => {
  const vector = fixture.vectors[0];
  const rightId = BigInt(vector.right_id);
  assert.throws(() => miningPowHash("OUTBE_NOD_MINING", rightId, vector.owner, 0n, 1n), /19 bytes/);
  assert.throws(() => miningPowHash(vector.domain_tag, rightId, "0x1111" as Address, 0n, 1n), /20 bytes/);
  assert.throws(() => miningPowHash(vector.domain_tag, -1n, vector.owner, 0n, 1n), /rightId/);
  assert.throws(() => miningPowHash(vector.domain_tag, rightId, vector.owner, 1n << 64n, 1n), /miningSequence/);
  assert.throws(() => miningPowHash(vector.domain_tag, rightId, vector.owner, 0n, 1n << 64n), /nonce/);
});
