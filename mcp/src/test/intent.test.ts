import assert from "node:assert/strict";
import test from "node:test";
import { encodeAbiParameters, pad, stringToHex, zeroAddress } from "viem";
import { ORDER_DATA_TYPE_HASH, encodeOrderData } from "../intent/format.js";
import { DEFAULT_ROUTER, ROUTER_ABI } from "../intent/registry.js";
import { SIGNER, startHarness } from "./harness.js";

const ORDER_ID = `0x${"44".repeat(32)}`;

function openOrder(destinationDomain: number) {
  return encodeAbiParameters(
    [{ type: "bytes32" }, { type: "bytes" }],
    [
      ORDER_DATA_TYPE_HASH,
      encodeOrderData({
        sender: pad(SIGNER),
        recipient: pad(SIGNER),
        inputToken: pad(zeroAddress),
        outputToken: pad(zeroAddress),
        amountIn: 5n,
        amountOut: 5n,
        senderNonce: 1n,
        originDomain: 97,
        destinationDomain,
        destinationSettler: pad(DEFAULT_ROUTER),
        fillDeadline: 1_760_000_100,
        data: "0x",
      }),
    ],
  );
}

test("an order to an unknown chain reports its destination as unknown instead of reading the origin", async () => {
  const harness = await startHarness((chain) => {
    chain.register(ROUTER_ABI, DEFAULT_ROUTER);
    chain.reply("openOrders", openOrder(1));
    chain.reply("orderStatus", stringToHex("OPENED", { size: 32 }));
    chain.reply("destinationOrderStatus", stringToHex("FILLED", { size: 32 }));
  });
  try {
    const { isError, text } = await harness.call("intent_order_track", { order_id: ORDER_ID, chain: "bsc-testnet" });
    assert(!isError, text);
    const out = JSON.parse(text);
    assert.equal(out.destinationNetwork, "chainId:1");
    assert.equal(out.destinationStatus, "UNKNOWN");
    assert.equal(out.userBalances.outputOnDest, null);
    assert.notEqual(out.phase, "FILLED");
  } finally {
    await harness.close();
  }
});
