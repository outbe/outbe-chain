import assert from "node:assert/strict";
import test from "node:test";
import { chacha20poly1305 } from "@noble/ciphers/chacha";
import { x25519 } from "@noble/curves/ed25519";
import { hkdf } from "@noble/hashes/hkdf";
import { sha256 } from "@noble/hashes/sha256";
import { type Hex, bytesToBigInt, bytesToHex, hexToBytes, numberToBytes } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { buildPayload } from "../crypto.js";
import { CONTRACTS } from "../registry.js";
import type { FakeChain } from "./fake-chain.js";
import { creatorKey } from "../tribute/creator-key.js";
import { SIGNER, SIGNER_KEY, startHarness } from "./harness.js";

const HASH = `0x${"44".repeat(32)}`;
const OFFER = {
  zk_proof: "0x0102",
  zk_merkle_root: HASH,
  signature: "0x0304",
  l2_chain_id: 57005,
  circuit_version: "1.1.0",
  tribute_draft_id: HASH,
  su_hashes: [HASH],
};
const RECIPIENT = new Uint8Array(32).fill(7);
const OUTBE_CHAIN_ID = 54_322_345;
const SIGNER_CREATOR_KEY = "0x9931b20b03318baacf6837eeed6eec3a05497b8b1ae07b1b8406fa4d022de02f";

async function offer(prepare: (chain: FakeChain) => void, args: Record<string, unknown> = {}) {
  const harness = await startHarness((chain) => {
    for (const entry of Object.values(CONTRACTS)) chain.register(entry.abi, entry.address);
    chain.reply("tributeOfferPublicKey", bytesToBigInt(x25519.getPublicKey(RECIPIENT)));
    prepare(chain);
  });
  try {
    const result = await harness.call("tribute_offer", { ...OFFER, ...args });
    return { ...result, sent: harness.chain.sent };
  } finally {
    await harness.close();
  }
}

function decryptOffer(sent: { args: unknown }[]): Uint8Array {
  const [cipherText, nonce, ephemeralPubkey] = sent[0].args as [Hex, Hex, string];
  const shared = x25519.getSharedSecret(RECIPIENT, numberToBytes(BigInt(ephemeralPubkey), { size: 32 }));
  const salt = new Uint8Array(32);
  salt.set(new TextEncoder().encode("outbe/tribute/offer-salt/v1"));
  const key = hkdf(sha256, shared, salt, new TextEncoder().encode("tribute-factory-encryption"), 32);
  return chacha20poly1305(key, hexToBytes(nonce)).decrypt(hexToBytes(cipherText));
}

test("tribute_offer encrypts a payload the offer key's holder decrypts", async () => {
  const { isError, text, sent } = await offer(() => {}, { worldwide_day: 20261009, amount: "250", amount_micro: "5" });
  assert(!isError, text);
  const plaintext = decryptOffer(sent);
  const expected = buildPayload({
    creator: SIGNER,
    amount_base: "250",
    amount_micro: "5",
    tribute_draft_id: HASH,
    su_hashes: [HASH],
    creator_public_key: SIGNER_CREATOR_KEY,
  });
  assert.deepEqual(plaintext, expected);
  assert.equal(JSON.parse(text).creatorPublicKey, SIGNER_CREATOR_KEY);
});

test("the creator key is a fixed function of the signer and the chain", async () => {
  const account = privateKeyToAccount(SIGNER_KEY);
  const key = await creatorKey(account, OUTBE_CHAIN_ID);
  assert.equal(key.publicKey, SIGNER_CREATOR_KEY);
  assert.deepEqual(await creatorKey(account, OUTBE_CHAIN_ID), key);
  assert.equal(bytesToHex(x25519.getPublicKey(key.secret)), key.publicKey);
  assert.notEqual((await creatorKey(account, 97)).publicKey, key.publicKey);
  const other = privateKeyToAccount(`0x${"11".repeat(32)}`);
  assert.notEqual((await creatorKey(other, OUTBE_CHAIN_ID)).publicKey, key.publicKey);
});

test("tribute_offer sends a given creator key instead of the derived one", async () => {
  const given = `0x${"55".repeat(32)}`;
  const { isError, text, sent } = await offer(() => {}, { worldwide_day: 20261009, creator_public_key: given });
  assert(!isError, text);
  const payload = JSON.parse(new TextDecoder().decode(decryptOffer(sent)));
  assert.equal(payload.creator_public_key, given);
});

test("tribute_offer refuses before the offer key exists", async () => {
  const { isError, text, sent } = await offer((chain) => chain.reply("isBootstrapped", false));
  assert(isError);
  assert.match(text, /TeeRegistry not bootstrapped/);
  assert.equal(sent.length, 0);
});

test("tribute_offer refuses when no WorldwideDay is offering", async () => {
  const { isError, text, sent } = await offer((chain) => chain.reply("getWorldwideDaysByStatus", []));
  assert(isError);
  assert.match(text, /no WorldwideDay is currently in OFFERING status/);
  assert.equal(sent.length, 0);
});
