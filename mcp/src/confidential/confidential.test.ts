import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { type Address, bytesToHex, hexToBytes } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { createCtx } from "../chain.js";
import { contextNetwork } from "../net/resolver.js";
import { CONTRACTS } from "../registry.js";
import { FakeChain } from "../test/fake-chain.js";
import { MODIFY_KEY, fakeEnclave, promisBlob } from "../test/fake-enclave.js";
import { OUTBE_RPC, SIGNER, SIGNER_KEY, startHarness } from "../test/harness.js";
import { withAuthorization } from "./authorize.js";
import { openGratis, openPromis } from "./balances.js";
import { fidelityQueryAuth, fidelityQueryMessage } from "./fidelity.js";
import { openNodAmount } from "./nod.js";
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

test("balance blobs open to the amounts of the shared vectors", () => {
  const v = vectors.balances;
  const view = hexToBytes(v.viewKey);
  assert.equal(openPromis(view, v.account, v.promis.blob), BigInt(v.promis.amount));
  for (const g of v.gratis) assert.equal(openGratis(view, v.account, g.field, g.blob), BigInt(g.amount));
  assert.equal(openPromis(view, v.account, "0x"), 0n);
});

test("the signer's balance is shown decrypted, another account's stays ciphertext", async () => {
  const harness = await startHarness((chain) => {
    for (const entry of Object.values(CONTRACTS)) chain.register(entry.abi, entry.address);
    fakeEnclave(chain);
    chain.reply("balanceOf", (args: readonly unknown[]) => promisBlob(args[0] as Address, 2_500_000n), CONTRACTS.promis.address);
  });
  try {
    const own = JSON.parse((await harness.call("promis_balance", { account: SIGNER })).text);
    assert.deepEqual(own.decrypted, { balance: { raw: "2500000", value: "2.5" } });
    const other = JSON.parse((await harness.call("promis_balance", { account: `0x${"22".repeat(20)}` })).text);
    assert.equal(other.decrypted, undefined);
  } finally {
    await harness.close();
  }
});

test("a Nod's Gratis amount opens with the creator key, as the enclave encrypted it", () => {
  const v = vectors.nod;
  const nod = {
    nodId: BigInt(v.nodId),
    owner: v.owner,
    worldwideDay: v.worldwideDay,
    leagueId: v.leagueId,
    entryPriceMinor: BigInt(v.entryPriceMinor),
    issuanceCurrency: v.issuanceCurrency,
    referenceCurrency: v.referenceCurrency,
    chainId: BigInt(v.chainId),
    encryptionBinding: v.encryptionBinding,
    encryptedCreatorPublicKey: v.encryptedCreatorPublicKey,
    encryptedGratisAmount: v.encryptedGratisAmount,
  };
  const amount = openNodAmount(hexToBytes(v.creatorSecret), hexToBytes(v.networkPublicKey), nod);
  assert.equal(amount, BigInt(v.gratisAmount));
  assert.throws(() => openNodAmount(new Uint8Array(32).fill(0x32), hexToBytes(v.networkPublicKey), nod));
});

test("the Fidelity query authorization matches the bytes the enclave verifies", async () => {
  const v = vectors.fidelityQuery;
  const account = privateKeyToAccount(vectors.deriveKeys.signerKey);
  assert.equal(account.address, v.account);
  assert.equal(bytesToHex(fidelityQueryMessage(Number(v.chainId), v.account, BigInt(v.expiry))), v.message);
  assert.equal(await fidelityQueryAuth(account, Number(v.chainId), BigInt(v.expiry)), v.signature);
});
