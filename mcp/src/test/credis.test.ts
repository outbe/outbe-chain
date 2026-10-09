import assert from "node:assert/strict";
import test from "node:test";
import { type Hex, encodeAbiParameters, encodeEventTopics, zeroAddress } from "viem";
import { CONTRACTS, resolveContract } from "../registry.js";
import type { FakeChain } from "./fake-chain.js";
import { SIGNER, startHarness } from "./harness.js";

const vaultRouter = resolveContract("vaultrouter");
const gratisFactory = resolveContract("gratisfactory");

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
    args: { id: 41n, smartAccount: SIGNER, cca: SIGNER },
  });
  const data = encodeAbiParameters(
    [{ type: "address" }, { type: "address" }, { type: "uint256" }, { type: "uint64" }],
    [zeroAddress, zeroAddress, 300n, 1_760_000_900n],
  );
  const { out } = await call(
    "credis_reserve",
    { smart_account: SIGNER, source: SIGNER, asset: SIGNER, amount: "300", reference_currency: 840 },
    (chain) => chain.receiptLogs("reserveStables", [{ address: vaultRouter.address, topics: topics as Hex[], data }]),
  );
  assert.equal(out.reservationId, "41");
});

test("credis_issue stakes the reserved collateral and returns the position id", async () => {
  const topics = encodeEventTopics({
    abi: gratisFactory.abi,
    eventName: "PledgeSentToCredis",
    args: { reservationId: 41n, positionId: 77n },
  });
  const { out, sent } = await call("credis_issue", { reservation_id: "41" }, (chain) => {
    chain.reply("pledgeOf", [SIGNER, 500_000n]);
    chain.reply("reservationOf", { ...sampleReservation(), gratisMinor: 500_000n });
    chain.receiptLogs("issueCredis", [{ address: gratisFactory.address, topics: topics as Hex[], data: "0x" }]);
  });
  assert.equal(out.positionId, "77");
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

function sampleReservation() {
  return {
    asset: SIGNER,
    amount: 300n,
    smartAccount: SIGNER,
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
