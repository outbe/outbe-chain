import assert from "node:assert/strict";
import { test } from "node:test";
import { createCipheriv, createHash, createHmac, hkdfSync, randomBytes } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { x25519 } from "@noble/curves/ed25519";
import { ethers } from "ethers";
import { assertSettleable, CHAIN_ID, Journal, parseGemId, type Transport } from "./rudis-gems.js";
import { deriveKeys, findPowNonce, modifyMac, PromisOp } from "./confidential.js";

test("GEM IDs remain exact uint256 values and reject malformed input", () => {
  assert.equal(parseGemId(ethers.MaxUint256.toString()), ethers.MaxUint256);
  assert.equal(parseGemId("0x1234"), 4660n);
  for (const invalid of ["0", "-1", "1.5", "1e6", "0x", (ethers.MaxUint256 + 1n).toString()]) {
    assert.throws(() => parseGemId(invalid));
  }
});

test("only Qualified / in-window Called can settle, including the deadline second", () => {
  const gem = { state: 2n, calledAt: 100n, callNoticePeriod: 10n };
  assertSettleable(gem, 110n);
  assert.throws(() => assertSettleable(gem, 111n), /expired/);
  assertSettleable({ ...gem, state: 1n }, 999n);
  for (const state of [0n, 3n, 4n]) assert.throws(() => assertSettleable({ ...gem, state }, 100n));
});

test("signed tx is durable before broadcast; timeout resumes the SAME tx once", async () => {
  const dir = mkdtempSync(join(tmpdir(), "rudis-gems-"));
  try {
    const path = join(dir, "operation.json");
    const wallet = ethers.Wallet.createRandom();
    const identity = { chainId: CHAIN_ID.toString(), owner: wallet.address, gemId: "42", amount: "1000001" };
    const raw = await wallet.signTransaction({ chainId: CHAIN_ID, nonce: 0, gasLimit: 21000n,
      gasPrice: 1n, to: wallet.address, value: 0n, type: 0 });
    let prepares = 0;
    let broadcasts = 0;
    let confirmed = false;
    const success = { status: 1, logs: [] };
    const transport: Transport = {
      receipt: async () => confirmed ? success : null,
      broadcast: async bytes => {
        broadcasts++;
        assert.equal(bytes, raw);
        assert.equal(JSON.parse(readFileSync(path, "utf8")).transactions.mine.raw, bytes);
      },
      wait: async () => null,
    };
    const prepare = async () => { prepares++; return raw; };
    await assert.rejects(new Journal(path, identity).send("mine", prepare, transport), /not confirmed/);
    assert.equal(statSync(path).mode & 0o777, 0o600);
    assert.ok(!readFileSync(path, "utf8").includes(wallet.privateKey));
    // The first broadcast eventually mined while the client was disconnected.
    confirmed = true;
    await new Journal(path, identity).send("mine", prepare, transport);
    await new Journal(path, identity).send("mine", prepare, transport);
    assert.equal(prepares, 1);
    assert.equal(broadcasts, 1);
    assert.throws(() => new Journal(path, { ...identity, gemId: "43" }), /does not match/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("a reverted receipt stops the chain and permits a fresh attempt on next run", async () => {
  const dir = mkdtempSync(join(tmpdir(), "rudis-revert-"));
  try {
    const journal = new Journal(join(dir, "operation.json"),
      { chainId: CHAIN_ID.toString(), owner: ethers.Wallet.createRandom().address, gemId: "1", amount: "1" });
    const transport: Transport = {
      receipt: async () => ({ status: 0, logs: [] }),
      broadcast: async () => assert.fail("already mined"), wait: async () => assert.fail("already mined"),
    };
    await assert.rejects(journal.send("settle", async () => "0x1234", transport), /reverted/);
    assert.deepEqual(journal.data.transactions, {});
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("PoW and mint/burn MAC bind large amounts, operation, fresh nonce and Rehearsal", () => {
  const id = ethers.MaxUint256;
  const nonce = findPowNonce(id);
  const work = Buffer.concat([Buffer.from(ethers.getBytes(ethers.toBeHex(id, 32))),
    Buffer.from(ethers.getBytes(ethers.toBeHex(nonce, 8)))]);
  assert.equal(createHash("sha256").update(work).digest()[0], 0);
  const key = randomBytes(32);
  const owner = ethers.Wallet.createRandom().address;
  const amount = 2684948507639n;
  const mint = modifyMac(key, owner, PromisOp.Mint, amount, 7n, CHAIN_ID, "Promis");
  const burn = modifyMac(key, owner, PromisOp.Burn, amount, 8n, CHAIN_ID, "Promis");
  // Contract wire encoding is packed, not standard ABI word padding for the
  // address, discriminant and uint64 nonce.
  const preimage = ethers.solidityPacked(["string", "address", "uint8", "uint256", "uint64", "uint256"],
    ["outbe/promis/modify/v1", owner, 1, amount, 8, CHAIN_ID]);
  assert.equal(burn, ethers.hexlify(createHmac("sha256", key).update(ethers.getBytes(preimage)).digest()));
  assert.notEqual(mint, burn);
  assert.notEqual(burn, modifyMac(key, owner, PromisOp.Burn, amount, 7n, CHAIN_ID, "Promis"));
  assert.notEqual(burn, modifyMac(key, owner, PromisOp.Burn, amount, 8n, CHAIN_ID + 1n, "Promis"));
});

test("Rudis key derivation verifies wallet proof and decrypts enclave X25519 envelope", async () => {
  const secretKeys = randomBytes(64);
  class EnclaveRpc extends ethers.JsonRpcProvider {
    override async send(method: string, params: Array<unknown> | Record<string, unknown>): Promise<unknown> {
      assert.equal(method, "rudis_deriveKeys");
      assert.ok(Array.isArray(params));
      const [ledger, account, publicKey, signature] = params as string[];
      assert.equal(ledger, "Promis");
      const message = ethers.concat([ethers.toUtf8Bytes("outbe/promis/derive-keys/v1"), account, publicKey]);
      assert.equal(ethers.verifyMessage(ethers.getBytes(message), signature), account);
      const privateKey = x25519.utils.randomPrivateKey();
      const enclavePublic = x25519.getPublicKey(privateKey);
      const shared = x25519.getSharedSecret(privateKey, ethers.getBytes(publicKey));
      const key = Buffer.from(hkdfSync("sha256", shared, ethers.getBytes(publicKey), "outbe/tee/dkg-share/v1", 32));
      const nonce = randomBytes(12);
      const cipher = createCipheriv("chacha20-poly1305", key, nonce, { authTagLength: 16 });
      const sealed = Buffer.concat([cipher.update(secretKeys), cipher.final(), cipher.getAuthTag()]);
      return { sealed: ethers.hexlify(sealed), nonce: ethers.hexlify(nonce), enclaveEphemeralPubkey: ethers.hexlify(enclavePublic) };
    }
  }
  const provider = new EnclaveRpc();
  try {
    const wallet = new ethers.Wallet(ethers.Wallet.createRandom().privateKey, provider);
    const keys = await deriveKeys(wallet, "Promis", "rudis_deriveKeys");
    assert.deepEqual(Buffer.from(keys.viewKey), secretKeys.subarray(0, 32));
    assert.deepEqual(Buffer.from(keys.modifyKey), secretKeys.subarray(32));
  } finally { provider.destroy(); }
});
