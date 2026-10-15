import assert from "node:assert/strict";
import test from "node:test";
import { type Hex, encodeAbiParameters, encodeEventTopics, zeroAddress } from "viem";
import { OP, chainIdWord, modifyMac } from "../confidential/mac.js";
import { CONTRACTS, resolveContract } from "../registry.js";
import type { FakeChain } from "./fake-chain.js";
import { MODIFY_KEY, fakeEnclave } from "./fake-enclave.js";
import { SIGNER, startHarness } from "./harness.js";

const vaultRouter = resolveContract("vaultrouter");
const credisFactory = resolveContract("credisfactory");

async function call(tool: string, args: Record<string, unknown>, prepare: (chain: FakeChain) => void = () => {}) {
  const harness = await startHarness((chain) => {
    for (const entry of Object.values(CONTRACTS)) chain.register(entry.abi, entry.address);
    prepare(chain);
  });
  try {
    const { text, isError } = await harness.call(tool, args);
    assert(!isError, text);
    return { out: JSON.parse(text), sent: harness.chain.sent };
  } finally {
    await harness.close();
  }
}

test("credis_reserve returns the reservation id its event carries", async () => {
  const topics = encodeEventTopics({
    abi: vaultRouter.abi,
    eventName: "ReservationCreated",
    args: { id: 41n, credisOwner: SIGNER, cca: SIGNER },
  });
  const data = encodeAbiParameters(
    [{ type: "address" }, { type: "address" }, { type: "uint256" }, { type: "uint64" }],
    [zeroAddress, zeroAddress, 300n, 1_760_000_900n],
  );
  const { out } = await call(
    "credis_reserve",
    { credis_owner: SIGNER, source: SIGNER, asset: SIGNER, amount: "300", reference_currency: 840 },
    (chain) => chain.receiptLogs("reserveStables", [{ address: vaultRouter.address, topics: topics as Hex[], data }]),
  );
  assert.equal(out.reservationId, "41");
});

test("credis_issue stakes the reserved collateral and returns the Credis id", async () => {
  const topics = encodeEventTopics({
    abi: credisFactory.abi,
    eventName: "CredisIssued",
    args: { credisId: 77n, owner: SIGNER, cca: SIGNER },
  });
  const data = encodeAbiParameters(
    [{ type: "address" }, { type: "uint256" }, { type: "uint256" }],
    [SIGNER, 300n, 500_000n],
  );
  const { out, sent } = await call("credis_issue", { reservation_id: "41" }, (chain) => {
    chain.reply("pledgeOf", [SIGNER, 500_000n]);
    chain.reply("reservationOf", { ...sampleReservation(), gratisMinor: 500_000n });
    chain.receiptLogs("issueCredis", [{ address: credisFactory.address, topics: topics as Hex[], data }]);
  });
  assert.equal(out.credisId, "77");
  assert.deepEqual(sent.at(-1)?.args, [41n]);
  assert.equal(sent.at(-1)?.value, "500000000000000000");
});

test("credis_issue refuses a reservation nobody pledged", async () => {
  const harness = await startHarness((chain) => {
    for (const entry of Object.values(CONTRACTS)) chain.register(entry.abi, entry.address);
    chain.reply("pledgeOf", [zeroAddress, 0n]);
  });
  try {
    const { text, isError } = await harness.call("credis_issue", { reservation_id: "41" });
    assert(isError);
    assert.match(text, /reservation 41 has no pledge; pledge it with gratis_pledge first/);
    assert.equal(harness.chain.sent.length, 0);
  } finally {
    await harness.close();
  }
});

test("credis_forfeit returns what the forfeit burned and wrote off", async () => {
  const credis = resolveContract("credis");
  const topics = encodeEventTopics({
    abi: credis.abi,
    eventName: "CredisForfeited",
    args: { credisId: 77n, cca: SIGNER },
  });
  const data = encodeAbiParameters([{ type: "uint256" }, { type: "uint256" }], [500_000n, 300n]);
  const { out, sent } = await call("credis_forfeit", { credis_id: "77" }, (chain) => {
    chain.receiptLogs("forfeitCredis", [{ address: credis.address, topics: topics as Hex[], data }]);
  });
  assert.equal(out.gratisBurnedMinor, "500000");
  assert.equal(out.principalWrittenOffMinor, "300");
  assert.deepEqual(sent.at(-1)?.args, [77n]);
});

test("gratis_pledge authorizes the pledge of the reservation's Gratis by its source", async () => {
  const { out, sent } = await call("gratis_pledge", { reservation_id: "41" }, (chain) => {
    fakeEnclave(chain);
    chain.reply("reservationOf", { ...sampleReservation(), gratisMinor: 500_000n });
    chain.reply("opNonceOf", 2n, CONTRACTS.gratis.address);
  });
  const mac = modifyMac(MODIFY_KEY, {
    ledger: "Gratis",
    account: SIGNER,
    op: OP.Gratis.Pledge,
    amount: 500_000n,
    opNonce: 2n,
    chainId: chainIdWord(54_322_345),
  });
  assert.deepEqual(sent.at(-1)?.args, [41n, { mac, opNonce: 2n }]);
  assert.equal(out.status, "success");
});

test("gratis_pledge refuses a reservation another source pledges or a half-given authorization", async () => {
  const harness = await startHarness((chain) => {
    for (const entry of Object.values(CONTRACTS)) chain.register(entry.abi, entry.address);
    fakeEnclave(chain);
    chain.reply("reservationOf", { ...sampleReservation(), source: vaultRouter.address });
  });
  try {
    const other = await harness.call("gratis_pledge", { reservation_id: "41" });
    assert.match(other.text, /reservation 41 is pledged by 0x0000000000000000000000000000000000001017, not the signer/);
    const half = await harness.call("gratis_pledge", { reservation_id: "41", mac: `0x${"22".repeat(32)}` });
    assert.match(half.text, /pass mac and op_nonce together, or neither/);
    assert.equal(harness.chain.sent.length, 0);
  } finally {
    await harness.close();
  }
});

function sampleReservation() {
  return {
    asset: SIGNER,
    amount: 300n,
    credisOwner: SIGNER,
    cca: SIGNER,
    vault: SIGNER,
    expiresAt: 1_760_000_900n,
    gratisMinor: 0n,
    snapshotId: 1n,
    entryPriceMinor: 2_000_000n,
    valuationPriceMinor: 2_000_000n,
    policyRate: 43_000n,
    issuanceCurrency: 840,
    assetDecimals: 6,
    referenceCurrency: 840,
    callAnchorPriceMinor: 2_000_000n,
    source: SIGNER,
  };
}
