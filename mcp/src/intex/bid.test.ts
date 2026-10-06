import assert from "node:assert/strict";
import test from "node:test";
import { hashTypedData, recoverTypedDataAddress } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { commitHash, revealBidTypedData } from "./bid.js";

const KEY = `0x${"100".padStart(64, "0")}` as const;
const account = privateKeyToAccount(KEY);
const params = {
  chainId: 56,
  verifyingContract: "0x000000000000000000000000000000000000cafe" as const,
  worldwideDay: 20260108,
  bidder: account.address,
  units: 5,
  bidRate: 1100,
  issuanceCurrency: 840,
  referenceCurrency: 840,
};
const DIGEST = "0x79d282877f73104ba843ec1a87de49b71c0069f3d25da66f0537abb0c9a3da61";
const SIGNATURE = "0x8bb0a9136c704f6cd320eccaa0c7d00fbf5148e6973252284823c6aa66da0c916c3619e2142d3766b63fb6bc8a27e9a8f2454080239370f759e8c81109741ec91c";
const COMMIT = "0xe5530a4bac07b5ece7b7aab6d17308dac486432e646e13208cef87b9cafc961a";

test("canonical units matches the Solidity digest, signature and commit vector", async () => {
  const typed = revealBidTypedData(params);
  assert.deepEqual(typed.types.RevealBid[2], { name: "units", type: "uint16" });
  assert.equal(hashTypedData(typed), DIGEST);
  const signature = await account.signTypedData(typed);
  assert.equal(signature, SIGNATURE);
  assert.equal(commitHash(signature), COMMIT);
  assert.equal(await recoverTypedDataAddress({ ...typed, signature }), account.address);
});

test("the legacy quantity signature cannot authorize a canonical units bid", async () => {
  const typed = revealBidTypedData(params);
  assert.deepEqual(typed.types.RevealBid[2], { name: "units", type: "uint16" });
  const legacy = {
    ...typed,
    types: { RevealBid: [
      { name: "worldwideDay", type: "uint32" },
      { name: "bidder", type: "address" },
      { name: "quantity", type: "uint16" },
      { name: "bidRate", type: "uint32" },
      { name: "issuanceCurrency", type: "uint16" },
      { name: "referenceCurrency", type: "uint16" },
    ] as const },
    message: { ...typed.message, quantity: params.units },
  };
  const signature = await account.signTypedData(legacy);
  assert.notEqual(hashTypedData(legacy), DIGEST);
  assert.notEqual(await recoverTypedDataAddress({ ...typed, signature }), account.address);
});
