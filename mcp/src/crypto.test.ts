import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { buildPayload, canonicalAmountMicro } from "./crypto.js";

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
