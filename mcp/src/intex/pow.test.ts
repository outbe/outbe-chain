import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import type { Address, Hex } from "viem";
import { POW_DIFFICULTY, grindNonce } from "./pow.js";

// Computed outside both implementations. The node's intexfactory tests read the same file.
const fixture = JSON.parse(readFileSync(new URL("./pow.vectors.json", import.meta.url), "utf8")) as {
  difficulty_bytes: number;
  vectors: {
    owner: Address;
    promis_amount: string;
    series_id: Hex;
    seq: number;
    first_valid_nonce: string;
    first_valid_hash: Hex;
  }[];
};

test("the miner's difficulty is the node's", () => {
  assert.equal(POW_DIFFICULTY, fixture.difficulty_bytes);
});

test("the miner finds the node's first valid nonce and hash for every shared vector", () => {
  assert.ok(fixture.vectors.length > 0);
  for (const vector of fixture.vectors) {
    const solution = grindNonce(vector.owner, BigInt(vector.promis_amount), vector.series_id, vector.seq);
    assert.equal(solution.nonce, BigInt(vector.first_valid_nonce));
    assert.equal(solution.hash, vector.first_valid_hash);
  }
});
