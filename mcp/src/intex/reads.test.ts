import assert from "node:assert/strict";
import test from "node:test";
import type { FakeChain } from "../test/fake-chain.js";
import { startHarness } from "../test/harness.js";
import { AUCTION_ABI, intexAddress } from "./registry.js";

const WINDOW = { from_date: 20261007, to_date: 20261009, include_all: true };

async function active(prepare: (chain: FakeChain) => void) {
  const harness = await startHarness((chain) => {
    chain.register(AUCTION_ABI, intexAddress("bsc-testnet", "auction"));
    prepare(chain);
  });
  try {
    return await harness.call("auctions_active", WINDOW);
  } finally {
    await harness.close();
  }
}

test("a day whose auction reverts as missing is left out of the window", async () => {
  const { isError, text } = await active((chain) => chain.revert("getAuctionStage"));
  assert(!isError, text);
  assert.equal(JSON.parse(text).count, 0);
});

test("a node that cannot be reached fails the discovery instead of reporting no auctions", async () => {
  const { isError, text } = await active((chain) => chain.breakReads("getAuctionStage"));
  assert(isError);
  assert.match(text, /upstream node unavailable/);
});
