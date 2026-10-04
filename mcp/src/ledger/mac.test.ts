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
