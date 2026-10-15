import assert from "node:assert/strict";
import test from "node:test";
import { type AbiEvent, type AbiFunction, type AbiParameter, type Hex, encodeAbiParameters, encodeEventTopics } from "viem";
import { OP, chainIdWord, modifyMac } from "../confidential/mac.js";
import { grindMiningNonce } from "../pow.js";
import { ERC20_ABI, FACTORY_ABI, intexAddress } from "../intex/registry.js";
import { CONTRACTS } from "../registry.js";
import { type FakeChain, sample } from "./fake-chain.js";
import { MODIFY_KEY, fakeEnclave } from "./fake-enclave.js";
import { SIGNER, startHarness } from "./harness.js";

const ASSET = "0x2222222222222222222222222222222222222222";

/** `fn`'s struct result with `fields` set and every other field sampled. */
function struct(contract: string, fn: string, fields: Record<string, unknown>) {
  const item = CONTRACTS[contract].abi.find((i) => i.type === "function" && i.name === fn) as AbiFunction;
  const components = (item.outputs[0] as { components: readonly AbiParameter[] }).components;
  return Object.fromEntries(components.map((c) => [c.name, c.name! in fields ? fields[c.name!] : sample(c)]));
}

async function run(tool: string, args: Record<string, unknown>, prepare: (chain: FakeChain) => void) {
  const harness = await startHarness((chain) => {
    for (const entry of Object.values(CONTRACTS)) chain.register(entry.abi, entry.address);
    chain.register(FACTORY_ABI, intexAddress({ name: "outbe-testnet", isOutbe: true }, "factory"));
    chain.register(ERC20_ABI);
    fakeEnclave(chain);
    chain.reply("powDifficulty", 1);
    prepare(chain);
  });
  try {
    const { text, isError } = await harness.call(tool, args);
    return { text, isError, sent: harness.chain.sent };
  } finally {
    await harness.close();
  }
}

test("gem_promis_mine sends the owner's proof of work and Promis Mint authorization over the load", async () => {
  const { text, isError, sent } = await run("gem_promis_mine", { gem_id: "9" }, (chain) => {
    chain.reply("getGem", struct("gem", "getGem", { owner: SIGNER, state: 3, promisLoadMinor: 4_000_000n }));
    chain.reply("opNonceOf", 7n, CONTRACTS.promis.address);
  });
  assert(!isError, text);
  const mac = modifyMac(MODIFY_KEY, {
    ledger: "Promis",
    account: SIGNER,
    op: OP.Promis.Mint,
    amount: 4_000_000n,
    opNonce: 7n,
    chainId: chainIdWord(54_322_345),
  });
  assert.deepEqual(sent.at(-1)?.args, [9n, grindMiningNonce("Gem", 9n, SIGNER, 1), mac, 7n]);
});

test("gem_promis_mine refuses another owner's Gem and an unsettled one", async () => {
  const other = await run("gem_promis_mine", { gem_id: "9" }, (chain) =>
    chain.reply("getGem", struct("gem", "getGem", { owner: ASSET, state: 3 })),
  );
  assert.match(other.text, /gem 9 belongs to 0x2222222222222222222222222222222222222222, not the signer/);
  const issued = await run("gem_promis_mine", { gem_id: "9" }, (chain) =>
    chain.reply("getGem", struct("gem", "getGem", { owner: SIGNER, state: 0 })),
  );
  assert.match(issued.text, /gem 9 is not Settled/);
  assert.equal(other.sent.length + issued.sent.length, 0);
});

test("gem_settle approves GemFactory for the quote and settles at its snapshot", async () => {
  const { text, isError, sent } = await run("gem_settle", { gem_id: "9", asset: ASSET }, (chain) => {
    chain.reply("quoteSettlement", [840, 5_000_000n, 12n], CONTRACTS.gemfactory.address);
    chain.reply("allowance", 0n);
  });
  assert(!isError, text);
  assert.deepEqual(
    sent.map((s) => [s.function, s.args]),
    [
      ["approve", [CONTRACTS.gemfactory.address, 5_000_000n]],
      ["settleGem", [9n, ASSET, 12n]],
    ],
  );
});

test("nod_settle approves NodFactory for the quote and settles at its snapshot", async () => {
  const { text, isError, sent } = await run("nod_settle", { nod_id: "4", asset: ASSET }, (chain) => {
    chain.reply("quoteSettlement", [978, 2_000_000n, 15n], CONTRACTS.nodfactory.address);
    chain.reply("allowance", 0n);
  });
  assert(!isError, text);
  assert.deepEqual(
    sent.map((s) => [s.function, s.args]),
    [
      ["approve", [CONTRACTS.nodfactory.address, 2_000_000n]],
      ["settleNod", [4n, ASSET, 15n]],
    ],
  );
});

test("agentreward_claim returns the id of the Gem it issued", async () => {
  const topics = encodeEventTopics({
    abi: CONTRACTS.gemfactory.abi,
    eventName: "GemIssued",
    args: { gemId: 31n, bucketKey: `0x${"00".repeat(32)}` },
  });
  const issued = CONTRACTS.gemfactory.abi.find((i) => i.type === "event" && i.name === "GemIssued") as AbiEvent;
  const data = encodeAbiParameters(
    issued.inputs.filter((i) => !i.indexed),
    issued.inputs.filter((i) => !i.indexed).map((i) => sample(i)),
  );
  const harness = await startHarness((chain) => {
    for (const entry of Object.values(CONTRACTS)) chain.register(entry.abi, entry.address);
    chain.receiptLogs("claimReward", [{ address: CONTRACTS.gemfactory.address, topics: topics as Hex[], data }]);
  });
  try {
    const { text, isError } = await harness.call("agentreward_claim", { pool: 0, issuance_currency: 840, reference_currency: 840 });
    assert(!isError, text);
    assert.equal(JSON.parse(text).gemId, "31");
  } finally {
    await harness.close();
  }
});
