import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { buildPayload, canonicalAmountMicro, openConfidentialReply, encryptPledgeCredential } from "./crypto.js";

const DRAFT_ID = `0x${"11".repeat(32)}`;
const SU_HASHES = [`0x${"22".repeat(32)}`, `0x${"33".repeat(32)}`];

function decodePayload(amount_base: string, amount_micro = "0"): Record<string, unknown> {
  return JSON.parse(
    new TextDecoder().decode(
      buildPayload({
        creator: "0x01",
        amount_base,
        amount_micro,
        tribute_draft_id: DRAFT_ID,
        su_hashes: SU_HASHES,
      }),
    ),
  ) as Record<string, unknown>;
}

const cases = JSON.parse(
  readFileSync(new URL("../../testdata/tribute/canonical-amounts-v1.json", import.meta.url), "utf8"),
) as {
  accepted_base: string[];
  rejected_base: string[];
  accepted_micro: string[];
  rejected_micro: string[];
};

test("tribute payload emits canonical base with a zero six-decimal remainder", () => {
  for (const amount of cases.accepted_base) {
    const payload = decodePayload(amount);
    assert.equal(payload.amount_base, amount);
    assert.equal(payload.amount_micro, "0");
    assert.equal(Object.hasOwn(payload, "amount_atto"), false);
  }
});

test("tribute payload rejects non-canonical unsigned u64 base values", () => {
  for (const amount of cases.rejected_base) {
    assert.throws(() => decodePayload(amount), `accepted amount_base ${JSON.stringify(amount)}`);
  }
});

test("tribute payload carries the proof-bound draft id and SU hashes verbatim", () => {
  // The enclave folds these into nft_hash, so a regenerated id/hash would make
  // every offer that presents a real proof fail its public-input check.
  const payload = decodePayload("100");
  assert.equal(payload.tribute_draft_id, DRAFT_ID);
  assert.deepEqual(payload.su_hashes, SU_HASHES);
});

test("tribute payload accepts only canonical six-decimal remainders", () => {
  for (const micro of cases.accepted_micro) {
    assert.equal(decodePayload("100", micro).amount_micro, micro);
    assert.equal(canonicalAmountMicro(micro), micro);
  }
  for (const micro of cases.rejected_micro) {
    assert.throws(() => decodePayload("100", micro), `accepted amount_micro ${JSON.stringify(micro)}`);
  }
});

test("confidential reply matches the Rust seal vector and binds its context", () => {
  const blob = Buffer.from("5e3c11467f6eafe4ccbd8126639d6b1c9c0be3a30038174bb9efa60eafcaf95a41848a5f763152bdcee283ff6027108d20a89ee3cf34a3f299fc964407c580a7c133c7200baac17df27364205c670b5f", "hex");
  const key = new Uint8Array(32).fill(1);
  assert.deepEqual(Array.from(openConfidentialReply(key, Buffer.from("context"), blob)), Array(32).fill(2));
  assert.throws(() => openConfidentialReply(key, Buffer.from("other context"), blob));
  blob[35] ^= 1;
  assert.throws(() => openConfidentialReply(key, Buffer.from("context"), blob));
});

test("pledge credentials are randomized and reject malformed fields", () => {
  const publicKey = new Uint8Array(32); publicKey[0] = 9;
  const args = [publicKey, new Uint8Array(32), new Uint8Array(32), new Uint8Array(20), new Uint8Array(32)] as const;
  const first = encryptPledgeCredential(...args);
  assert.equal(first.length, 208);
  assert.notDeepEqual(first, encryptPledgeCredential(...args));
  assert.throws(() => encryptPledgeCredential(publicKey, new Uint8Array(1), args[2], args[3], args[4]));
});
