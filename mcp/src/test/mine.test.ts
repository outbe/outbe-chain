import assert from "node:assert/strict";
import test from "node:test";
import { type Ledger, OP, chainIdWord, modifyMac } from "../confidential/mac.js";
import { CONTRACTS } from "../registry.js";
import type { FakeChain } from "./fake-chain.js";
import { MODIFY_KEY, fakeEnclave, gratisBlob, promisBlob } from "./fake-enclave.js";
import { SIGNER, startHarness } from "./harness.js";

const mac = (ledger: Ledger, op: number, amount: bigint, opNonce: bigint) =>
  modifyMac(MODIFY_KEY, { ledger, account: SIGNER, op, amount, opNonce, chainId: chainIdWord(54_322_345) });

async function run(tool: string, amount: string, prepare: (chain: FakeChain) => void) {
  const harness = await startHarness((chain) => {
    for (const entry of Object.values(CONTRACTS)) chain.register(entry.abi, entry.address);
    fakeEnclave(chain);
    chain.reply("opNonceOf", 3n, CONTRACTS.promis.address);
    chain.reply("opNonceOf", 8n, CONTRACTS.gratis.address);
    prepare(chain);
  });
  try {
    const { text, isError } = await harness.call(tool, { amount });
    return { text, isError, sent: harness.chain.sent };
  } finally {
    await harness.close();
  }
}

test("promis_gratis_mine binds one amount under both ledgers' own authorizations", async () => {
  const { text, isError, sent } = await run("promis_gratis_mine", "2000000", (chain) =>
    chain.reply("balanceOf", promisBlob(SIGNER, 5_000_000n), CONTRACTS.promis.address),
  );
  assert(!isError, text);
  assert.deepEqual(sent.at(-1)?.args, [
    2_000_000n,
    mac("Promis", OP.Promis.Burn, 2_000_000n, 3n),
    3n,
    mac("Gratis", OP.Gratis.Mint, 2_000_000n, 8n),
    8n,
  ]);
});

test("gratis_coen_mine burns liquid Gratis under the Gratis Burn authorization", async () => {
  const { text, isError, sent } = await run("gratis_coen_mine", "700000", (chain) =>
    chain.reply("balanceOf", gratisBlob(SIGNER, 0, 900_000n), CONTRACTS.gratis.address),
  );
  assert(!isError, text);
  assert.deepEqual(sent.at(-1)?.args, [700_000n, mac("Gratis", OP.Gratis.Burn, 700_000n, 8n), 8n]);
});

test("a mine above the decrypted balance is refused before anything is sent", async () => {
  const { text, isError, sent } = await run("promis_coen_mine", "6000000", (chain) =>
    chain.reply("balanceOf", promisBlob(SIGNER, 5_000_000n), CONTRACTS.promis.address),
  );
  assert(isError);
  assert.match(text, /Promis balance 5 is below the requested 6/);
  assert.equal(sent.length, 0);
});
