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
  quantity: 5,
  bidRate: 1100,
  issuanceCurrency: 840,
  referenceCurrency: 840,
};
const DIGEST = "0x35d31042ad5daa0f569876090530a923d6fb2a49bdf3f20874ed24358d35a45a";
const SIGNATURE = "0xafca4e3dfbb39f0850864924229d4d636ec7256d6358912a27e04aab0bb4c91428f1146fc84838a11695e3c030f55901af4185312485edbd1be25bd4212e97cc1c";
const COMMIT = "0x6bcf37c8a30f41999cc39e61771ca752662730d4fc87620703876283092c2101";

test("quantity matches the Solidity digest, signature and commit vector", async () => {
  const typed = revealBidTypedData(params);
  assert.deepEqual(typed.types.RevealBid[2], { name: "quantity", type: "uint16" });
  assert.equal(hashTypedData(typed), DIGEST);
  const signature = await account.signTypedData(typed);
  assert.equal(signature, SIGNATURE);
  assert.equal(commitHash(signature), COMMIT);
  assert.equal(await recoverTypedDataAddress({ ...typed, signature }), account.address);
});
