import assert from "node:assert/strict";
import test from "node:test";
import { AUCTION_ABI, ERC20_ABI, ESCROW_ABI, intexAddress } from "../intex/registry.js";
import type { FakeChain } from "../test/fake-chain.js";
import { startHarness } from "../test/harness.js";

const COMMIT = { worldwideDay: 20261009, units: 2, rate: "0.8", issuanceCurrency: 949, referenceCurrency: 840 };

async function commit(prepare: (chain: FakeChain) => void) {
  const harness = await startHarness((chain) => {
    chain.register(AUCTION_ABI, intexAddress("bsc-testnet", "auction"));
    chain.register(ESCROW_ABI, intexAddress("bsc-testnet", "escrow"));
    chain.register(ERC20_ABI);
    chain.reply("allowance", 0n);
    prepare(chain);
  });
  try {
    const result = await harness.call("auction_bid_commit", COMMIT);
    return { ...result, sent: harness.chain.sent.map((tx) => tx.function) };
  } finally {
    await harness.close();
  }
}

test("a short allowance is approved and mined before the call that spends it", async () => {
  const { isError, text, sent } = await commit(() => {});
  assert(!isError, text);
  assert.deepEqual(sent, ["approve", "commitBid"]);
  assert.match(text, /"autoApprove": \{\n\s+"txHash"/);
});

test("a reverted approval stops the call that would spend it", async () => {
  const { isError, text, sent } = await commit((chain) => chain.failReceipts("approve"));
  assert(isError);
  assert.match(text, /approve 0x[0-9a-f]{64} for 0x[0-9a-fA-F]{40} reverted/);
  assert.deepEqual(sent, ["approve"]);
});
