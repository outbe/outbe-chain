import assert from "node:assert/strict";
import test from "node:test";
import { chacha20poly1305 } from "@noble/ciphers/chacha";
import { x25519 } from "@noble/curves/ed25519";
import { hkdf } from "@noble/hashes/hkdf";
import { sha256 } from "@noble/hashes/sha256";
import { type Hex, bytesToBigInt, hexToBytes, numberToBytes } from "viem";
import { buildPayload } from "../crypto.js";
import { CONTRACTS } from "../registry.js";
import type { FakeChain } from "./fake-chain.js";
import { SIGNER, startHarness } from "./harness.js";

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

test("tribute_offer encrypts a payload the offer key's holder decrypts", async () => {
  const { isError, text, sent } = await offer(() => {}, { worldwide_day: 20261009, amount: "250", amount_micro: "5" });
  assert(!isError, text);
  const [cipherText, nonce, ephemeralPubkey] = sent[0].args as [Hex, Hex, string];
  const shared = x25519.getSharedSecret(RECIPIENT, numberToBytes(BigInt(ephemeralPubkey), { size: 32 }));
  const salt = new Uint8Array(32);
  salt.set(new TextEncoder().encode("outbe/tribute/offer-salt/v1"));
  const key = hkdf(sha256, shared, salt, new TextEncoder().encode("tribute-factory-encryption"), 32);
  const plaintext = chacha20poly1305(key, hexToBytes(nonce)).decrypt(hexToBytes(cipherText));
  const expected = buildPayload({
    creator: SIGNER,
    amount_base: "250",
    amount_micro: "5",
    tribute_draft_id: HASH,
    su_hashes: [HASH],
  });
  assert.deepEqual(plaintext, expected);
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
