import assert from "node:assert/strict";
import test from "node:test";
import type { FakeChain } from "../test/fake-chain.js";
import { startHarness } from "../test/harness.js";
import { AUCTION_ABI, ORIGIN_ROUTER_ABI, intexAddress } from "./registry.js";

const BSC = { name: "bsc-testnet", isOutbe: false };
const WINDOW = { from_date: 20261007, to_date: 20261009, include_all: true };

async function active(prepare: (chain: FakeChain) => void) {
  const harness = await startHarness((chain) => {
    chain.register(AUCTION_ABI, intexAddress(BSC, "auction"));
    chain.register(ORIGIN_ROUTER_ABI, intexAddress({ name: "outbe-testnet", isOutbe: true }, "originRouter"));
    chain.reply("targets", [97]);
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

test("a chain the origin router does not serve is refused before any auction read", async () => {
  const harness = await startHarness(
    (chain) => {
      chain.register(ORIGIN_ROUTER_ABI, intexAddress({ name: "outbe-testnet", isOutbe: true }, "originRouter"));
      chain.reply("targets", [97]);
    },
    { env: { OUTBE_RPC_11155111: "https://sepolia.example" } },
  );
  try {
    const { isError, text } = await harness.call("auctions_active", { network: "sepolia" });
    assert(isError);
    assert.match(text, /sepolia is not an Intex target; the origin router serves chains 97/);
  } finally {
    await harness.close();
  }
});
