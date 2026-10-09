import assert from "node:assert/strict";
import test from "node:test";
import { INTEX_ABI, intexAddress } from "../intex/registry.js";
import { startHarness } from "../test/harness.js";

test("outbe is the connected node whatever its chain id", async () => {
  const harness = await startHarness(
    (chain) => {
      chain.register(INTEX_ABI, intexAddress({ name: "outbe-424242", isOutbe: true }, "intex"));
      chain.reply("totalSeries", 0n);
    },
    { outbeChainId: 424_242 },
  );
  try {
    const { isError, text } = await harness.call("intex_series_list");
    assert(!isError, text);
    assert.equal(JSON.parse(text).network, "outbe-424242");
  } finally {
    await harness.close();
  }
});

test("another chain is reached through its OUTBE_RPC_<chainId>", async () => {
  const harness = await startHarness(() => {}, { env: { OUTBE_RPC_11155111: "https://sepolia.example" } });
  try {
    const { isError, text } = await harness.call("intex_series_info", { series: "20260212-TRY-U", network: "11155111" });
    assert(isError);
    assert.match(text, /Intex "intex" is not configured on "sepolia"/);
    const missing = await harness.call("intex_series_info", { series: "20260212-TRY-U", network: "8453" });
    assert.match(missing.text, /no RPC for chain 8453; set OUTBE_RPC_8453/);
  } finally {
    await harness.close();
  }
});
