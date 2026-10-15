import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { type Address, bytesToHex, hexToBytes } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { createCtx } from "../chain.js";
import { contextNetwork } from "../net/resolver.js";
import { CONTRACTS } from "../registry.js";
import { FakeChain } from "../test/fake-chain.js";
import { MODIFY_KEY, fakeEnclave } from "../test/fake-enclave.js";
import { OUTBE_RPC, SIGNER, SIGNER_KEY } from "../test/harness.js";
import { withAuthorization } from "./authorize.js";
import { deriveKeysMessage, unsealKeys } from "./keys.js";
import { type Ledger, OP, chainIdWord, modifyMac } from "./mac.js";

const vectors = JSON.parse(readFileSync(new URL("./vectors.json", import.meta.url), "utf8"));
const CHAIN_ID = 54_322_345;

test("the deriveKeys message and its signature match the shared vector", async () => {
  const v = vectors.deriveKeys;
  const account = privateKeyToAccount(v.signerKey);
  const message = deriveKeysMessage(v.ledger, account.address, hexToBytes(v.ephemeralPubkey));
  assert.equal(bytesToHex(message), v.message);
  assert.equal(await account.signMessage({ message: { raw: message } }), v.signature);
});

test("sealed keys open to the view and modify keys of the shared vector", () => {
  const v = vectors.sealedKeys;
  const keys = unsealKeys(hexToBytes(vectors.deriveKeys.ephemeralSecret), v);
  assert.equal(bytesToHex(keys.view), v.view);
  assert.equal(bytesToHex(keys.modify), v.modify);
});

test("modify MACs match the shared vectors", () => {
  for (const v of vectors.macs) {
    const request = {
      ledger: v.ledger as Ledger,
      account: v.account as Address,
      op: v.op,
      amount: BigInt(v.amount),
      opNonce: BigInt(v.opNonce),
      chainId: hexToBytes(v.chainId),
    };
    assert.equal(modifyMac(hexToBytes(v.modifyKey), request), v.mac);
  }
});

/** The keys cache per chain, so each test that counts derivations runs on its own chain id. */
async function outbe(prepare: (chain: FakeChain) => void, chainId = CHAIN_ID) {
  const chain = new FakeChain({ [OUTBE_RPC]: chainId }, SIGNER);
  for (const name of ["gratis", "promis"]) chain.register(CONTRACTS[name].abi, CONTRACTS[name].address);
  prepare(chain);
  const restore = chain.install();
  const ctx = await createCtx(OUTBE_RPC, SIGNER_KEY);
  return { ctx, network: contextNetwork(ctx), restore };
}

test("a write is authorized with the signer's modify key, its opNonce and the chain id", async () => {
  const calls: Ledger[] = [];
  const { ctx, network, restore } = await outbe((chain) => {
    fakeEnclave(chain, calls);
    chain.reply("opNonceOf", 4n, CONTRACTS.promis.address);
    chain.reply("opNonceOf", 11n, CONTRACTS.gratis.address);
  });
  try {
    const writes = [
      { ledger: "Promis" as const, op: OP.Promis.Burn, amount: 3_000_000n },
      { ledger: "Gratis" as const, op: OP.Gratis.Mint, amount: 3_000_000n },
    ];
    const auths = await withAuthorization(ctx, network, writes, async (a) => a);
    const chainId = chainIdWord(CHAIN_ID);
    assert.deepEqual(auths, [
      { mac: modifyMac(MODIFY_KEY, { ...writes[0], account: SIGNER, opNonce: 4n, chainId }), opNonce: 4n },
      { mac: modifyMac(MODIFY_KEY, { ...writes[1], account: SIGNER, opNonce: 11n, chainId }), opNonce: 11n },
    ]);
    await withAuthorization(ctx, network, writes, async (a) => a);
    assert.deepEqual(calls.sort(), ["Gratis", "Promis"]);
  } finally {
    restore();
  }
});

test("writes of one account and ledger never overlap", async () => {
  const { ctx, network, restore } = await outbe((chain) => fakeEnclave(chain, []), 424_243);
  try {
    const order: string[] = [];
    const write = { ledger: "Gratis" as const, op: OP.Gratis.Burn, amount: 1n };
    const run = (name: string) =>
      withAuthorization(ctx, network, [write], async () => {
        order.push(`${name} start`);
        await new Promise((resolve) => setTimeout(resolve, 20));
        order.push(`${name} end`);
      });
    await Promise.all([run("first"), run("second")]);
    assert.deepEqual(order, ["first start", "first end", "second start", "second end"]);
  } finally {
    restore();
  }
});

test("a rejected MAC re-derives the keys and sends once more", async () => {
  const calls: Ledger[] = [];
  const { ctx, network, restore } = await outbe((chain) => fakeEnclave(chain, calls), 424_244);
  try {
    let sends = 0;
    await withAuthorization(ctx, network, [{ ledger: "Promis", op: OP.Promis.Mint, amount: 5n }], async () => {
      sends += 1;
      if (sends === 1) throw new Error("execution reverted: invalid modify authorization");
    });
    assert.equal(sends, 2);
    assert.deepEqual(calls, ["Promis", "Promis"]);
  } finally {
    restore();
  }
});
