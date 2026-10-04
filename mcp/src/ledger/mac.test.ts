import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import type { Address, Hex } from "viem";
import { GRATIS_MODIFY_TAG, PROMIS_MODIFY_TAG, modifyMac } from "./mac.js";

// Computed outside both implementations; bin/outbe-tee-enclave tests read the same file.
const fixture = JSON.parse(readFileSync(new URL("./mac.vectors.json", import.meta.url), "utf8")) as {
  vectors: {
    ledger: "gratis" | "promis";
    domain_tag: string;
    modify_key: Hex;
    account: Address;
    op: string;
    op_tag: number;
    amount: string;
    op_nonce: number;
    chain_id: Hex;
    mac: Hex;
  }[];
};

test("the wallet reproduces every shared Gratis and Promis modify authorization", () => {
  assert.ok(fixture.vectors.length > 0);
  for (const vector of fixture.vectors) {
    assert.equal(vector.domain_tag, vector.ledger === "gratis" ? GRATIS_MODIFY_TAG : PROMIS_MODIFY_TAG);
    const mac = modifyMac(
      vector.domain_tag,
      vector.modify_key,
      vector.account,
      vector.op_tag,
      BigInt(vector.amount),
      BigInt(vector.op_nonce),
      vector.chain_id,
    );
    assert.equal(mac, vector.mac);
  }
});

test("a Gratis-tagged authorization never verifies as a Promis one", () => {
  const vector = fixture.vectors.find((entry) => entry.ledger === "promis");
  assert.ok(vector);
  const crossed = modifyMac(
    GRATIS_MODIFY_TAG,
    vector.modify_key,
    vector.account,
    vector.op_tag,
    BigInt(vector.amount),
    BigInt(vector.op_nonce),
    vector.chain_id,
  );
  assert.notEqual(crossed, vector.mac);
});

test("malformed byte fields are rejected instead of silently authorized", () => {
  const vector = fixture.vectors[0];
  const amount = BigInt(vector.amount);
  const nonce = BigInt(vector.op_nonce);
  assert.throws(() => modifyMac("outbe/other/modify/v1", vector.modify_key, vector.account, 0, amount, nonce, vector.chain_id), /domain tag/);
  assert.throws(() => modifyMac(vector.domain_tag, "0x5a5a" as Hex, vector.account, 0, amount, nonce, vector.chain_id), /32 bytes/);
  assert.throws(() => modifyMac(vector.domain_tag, vector.modify_key, "0x1111" as Address, 0, amount, nonce, vector.chain_id), /20 bytes/);
  assert.throws(() => modifyMac(vector.domain_tag, vector.modify_key, vector.account, 256, amount, nonce, vector.chain_id), /byte/);
  assert.throws(() => modifyMac(vector.domain_tag, vector.modify_key, vector.account, 0, -1n, nonce, vector.chain_id), /u256/);
  assert.throws(() => modifyMac(vector.domain_tag, vector.modify_key, vector.account, 0, amount, 1n << 64n, vector.chain_id), /u64/);
  assert.throws(() => modifyMac(vector.domain_tag, vector.modify_key, vector.account, 0, amount, nonce, "0x01" as Hex), /32 bytes/);
});
