// Cross-chain auction demo runbook: phase tasks. Each phase is resumable; all share one report
// keyed by --series-id. LayerZero fees are quoted on-chain per send.

import { task } from "hardhat/config";
import * as readline from "node:readline";
import { getContract, type Address } from "viem";

import { loadOrInitReport } from "../../scripts/demo/harness/report.js";
import { runStep, assertState } from "../../scripts/demo/harness/runner.js";
import { assertCrossChainState, awaitLzDelivery } from "../../scripts/demo/harness/lz.js";
import { resolveAddresses, requireAddress, type DemoNetwork } from "../../scripts/demo/harness/config.js";
import {
  getRunner,
  contractAt,
  buildAuctionConfig,
  buildIssuanceConfig,
  bnbChainId,
  ERC20_ABI,
  AUCTION_STAGE,
} from "../../scripts/demo/auction.js";
import { createCommitHash, createRevealSignature } from "../../scripts/auction/bidders.js";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const lazy = (fn: (args: any, hre: any) => Promise<void>) => async () => ({ default: fn });

interface CommonArgs {
  seriesId: string;
  outbeNetwork: string;
  bnbNetwork: string;
}
const opt = (name: string, description: string, defaultValue = "") => ({ name, description, defaultValue });
const commonOpts = <T extends ReturnType<typeof task>>(t: T): T =>
  t
    .addOption(opt("seriesId", "Auction seriesId (yyyymmdd); also the report run id"))
    .addOption(opt("outbeNetwork", "Outbe network", "outbeTestnetNew"))
    .addOption(opt("bnbNetwork", "BNB network", "bscTestnet")) as T;

const reportTitle = (seriesId: string): string => `Auction demo — series ${seriesId}`;

// --------------------------------------------------------------------------
// Phase 1: start (Outbe -> BNB)
// --------------------------------------------------------------------------
const startAction = async (args: CommonArgs) => {
  const outbeNet = args.outbeNetwork as DemoNetwork;
  const bnbNet = args.bnbNetwork as DemoNetwork;
  const seriesId = Number(args.seriesId);
  const outbe = getRunner(outbeNet);
  const bnb = getRunner(bnbNet);
  const aO = resolveAddresses(outbeNet);
  const aB = resolveAddresses(bnbNet);
  const desis = contractAt(outbe, "Desis", requireAddress(aO, "desis", outbeNet) as Address);
  const auction = contractAt(bnb, "IntexAuction", requireAddress(aB, "intexAuction", bnbNet) as Address);
  const report = loadOrInitReport(args.seriesId, reportTitle(args.seriesId));

  await runStep(
    { report, network: outbeNet, phase: "auction", publicClient: outbe.publicClient },
    "Desis.sendAuctionStageStart",
    `Start series ${seriesId} on Outbe and bridge AUCTION_STAGE_START to BNB.`,
    async () => {
      const config = buildAuctionConfig({ seriesId });
      // Prefunded float: send with value=0; OriginMessenger pays the LZ fee from its native balance.
      const txHash = (await desis.write.sendAuctionStageStart([config, "0x"])) as `0x${string}`;
      const lz = await awaitLzDelivery({
        srcNetwork: outbeNet,
        dstNetwork: bnbNet,
        srcPublic: outbe.publicClient,
        dstPublic: bnb.publicClient,
        srcOApp: requireAddress(aO, "originMessenger", outbeNet) as Address,
        dstOApp: requireAddress(aB, "targetMessenger", bnbNet) as Address,
      });
      // Proof of inbound dispatch: getAuctionInfo reverts AuctionNotFound until auctionStart runs.
      await assertCrossChainState({
        label: "BNB IntexAuction worldwideDayState",
        read: async () => {
          const info = (await auction.read.getAuctionInfo([seriesId])) as { worldwideDayState: number };
          return String(Number(info.worldwideDayState));
        },
        expected: "0", // Unknown — auction created, reveal signal not yet sent
        dstMessenger: requireAddress(aB, "targetMessenger", bnbNet) as Address,
        dstPublic: bnb.publicClient,
      });
      const stage = Number(await desis.read.getAuctionStage([seriesId]));
      return {
        txHash,
        lz,
        assertions: [assertState("Desis stage", "Started", AUCTION_STAGE[stage])],
        notes: "IntexAuction.auctionStart is invoked on BNB by the delivered message.",
      };
    },
  );
  console.log(`\nReport: reports/${args.seriesId}/report.md`);
};

// --------------------------------------------------------------------------
// Phase 2: commit (BNB) — the runner commits a sealed bid (stage CommittingBids)
// --------------------------------------------------------------------------
interface BidArgs extends CommonArgs {
  quantity: string;
  bidPrice: string;
}
const commitAction = async (args: BidArgs) => {
  const bnbNet = args.bnbNetwork as DemoNetwork;
  const seriesId = Number(args.seriesId);
  const quantity = Number(args.quantity);
  const bidPrice = BigInt(args.bidPrice);
  const bnb = getRunner(bnbNet);
  const aB = resolveAddresses(bnbNet);
  const auction = contractAt(bnb, "IntexAuction", requireAddress(aB, "intexAuction", bnbNet) as Address);
  const report = loadOrInitReport(args.seriesId, reportTitle(args.seriesId));
  const chainId = bnbChainId(bnbNet);
  const bidder = bnb.account.address;
  const ctx = { report, network: bnbNet, phase: "auction", publicClient: bnb.publicClient };

  await runStep(
    ctx,
    "IntexAuction.commitBid",
    `Commit a sealed bid for series ${seriesId} (BNB stage CommittingBids — must precede the reveal-stage signal).`,
    async () => {
      const commitHash = await createCommitHash(
        seriesId, bidder, BigInt(quantity), bidPrice, chainId, auction.address as Address, bnb.privateKey,
      );
      const txHash = (await auction.write.commitBid([seriesId, commitHash])) as `0x${string}`;
      return { txHash, notes: `commitHash ${commitHash}` };
    },
  );
};

// --------------------------------------------------------------------------
// Phase 3: reveal (Outbe -> BNB) — open the reveal/collection window (BNB -> RevealingBids)
// --------------------------------------------------------------------------
const revealAction = async (args: CommonArgs) => {
  const outbeNet = args.outbeNetwork as DemoNetwork;
  const bnbNet = args.bnbNetwork as DemoNetwork;
  const seriesId = Number(args.seriesId);
  const outbe = getRunner(outbeNet);
  const bnb = getRunner(bnbNet);
  const aO = resolveAddresses(outbeNet);
  const aB = resolveAddresses(bnbNet);
  const desis = contractAt(outbe, "Desis", requireAddress(aO, "desis", outbeNet) as Address);
  const auction = contractAt(bnb, "IntexAuction", requireAddress(aB, "intexAuction", bnbNet) as Address);
  const report = loadOrInitReport(args.seriesId, reportTitle(args.seriesId));

  await runStep(
    { report, network: outbeNet, phase: "auction", publicClient: outbe.publicClient },
    "Desis.sendAuctionStageReveal",
    `Open the reveal stage (green day) for series ${seriesId} and bridge it to BNB.`,
    async () => {
      // Prefunded float: OriginMessenger pays the LZ fee from its native balance.
      const txHash = (await desis.write.sendAuctionStageReveal([seriesId, true, "0x"])) as `0x${string}`;
      const lz = await awaitLzDelivery({
        srcNetwork: outbeNet,
        dstNetwork: bnbNet,
        srcPublic: outbe.publicClient,
        dstPublic: bnb.publicClient,
        srcOApp: requireAddress(aO, "originMessenger", outbeNet) as Address,
        dstOApp: requireAddress(aB, "targetMessenger", bnbNet) as Address,
      });
      // worldwideDayState is the only state startRevealingBidsStage writes; checking it directly
      // is time-independent and catches the drop-don't-block case where _lzReceive's try/catch
      // swallowed an inbound revert (Phase 4 would then fail with StageRequired(1, 0)).
      await assertCrossChainState({
        label: "BNB IntexAuction worldwideDayState",
        read: async () => {
          const info = (await auction.read.getAuctionInfo([seriesId])) as { worldwideDayState: number };
          return String(Number(info.worldwideDayState));
        },
        expected: "1", // Green
        dstMessenger: requireAddress(aB, "targetMessenger", bnbNet) as Address,
        dstPublic: bnb.publicClient,
      });
      const stage = Number(await desis.read.getAuctionStage([seriesId]));
      return { txHash, lz, assertions: [assertState("Desis stage", "Revealing", AUCTION_STAGE[stage])] };
    },
  );
};

// --------------------------------------------------------------------------
// Phase 4: reveal-bid (BNB) — reveal the committed bid, locking escrow (stage RevealingBids)
// --------------------------------------------------------------------------
const revealBidAction = async (args: BidArgs) => {
  const bnbNet = args.bnbNetwork as DemoNetwork;
  const seriesId = Number(args.seriesId);
  const quantity = Number(args.quantity);
  const bidPrice = BigInt(args.bidPrice);
  const bnb = getRunner(bnbNet);
  const aB = resolveAddresses(bnbNet);
  const auction = contractAt(bnb, "IntexAuction", requireAddress(aB, "intexAuction", bnbNet) as Address);
  const escrowAddr = requireAddress(aB, "escrowAdapter", bnbNet) as Address;
  const paymentToken = requireAddress(aB, "paymentToken", bnbNet) as Address;
  const report = loadOrInitReport(args.seriesId, reportTitle(args.seriesId));
  const chainId = bnbChainId(bnbNet);
  const bidder = bnb.account.address;
  const ctx = { report, network: bnbNet, phase: "auction", publicClient: bnb.publicClient };

  await runStep(ctx, "PaymentToken.approve", "Approve the escrow lock (quantity * bidPrice).", async () => {
    const erc20 = getContract({
      address: paymentToken,
      abi: ERC20_ABI,
      client: { public: bnb.publicClient, wallet: bnb.walletClient },
    });
    const amount = BigInt(quantity) * bidPrice;
    const txHash = (await erc20.write.approve([escrowAddr, amount])) as `0x${string}`;
    return { txHash, notes: `approved ${amount} to EscrowAdapter` };
  });

  await runStep(ctx, "IntexAuction.revealBid", "Reveal the committed bid (BNB stage RevealingBids; locks escrow).", async () => {
    const signature = await createRevealSignature(
      seriesId, bidder, BigInt(quantity), bidPrice, chainId, auction.address as Address, bnb.privateKey,
    );
    const txHash = (await auction.write.revealBid([seriesId, quantity, bidPrice, chainId, signature])) as `0x${string}`;
    return { txHash, notes: `revealed qty=${quantity} price=${bidPrice}` };
  });
};

// --------------------------------------------------------------------------
// Phase 5: clearing (Outbe -> BNB) — close reveals on BNB (-> Issuance) and persist supply+issuance.
// --------------------------------------------------------------------------
interface ClearingArgs extends CommonArgs {
  supply: string;
  bidPrice: string;
}
const clearingAction = async (args: ClearingArgs) => {
  const outbeNet = args.outbeNetwork as DemoNetwork;
  const bnbNet = args.bnbNetwork as DemoNetwork;
  const seriesId = Number(args.seriesId);
  const supplyIntex = Number(args.supply);
  const outbe = getRunner(outbeNet);
  const bnb = getRunner(bnbNet);
  const aO = resolveAddresses(outbeNet);
  const aB = resolveAddresses(bnbNet);
  const desis = contractAt(outbe, "Desis", requireAddress(aO, "desis", outbeNet) as Address);
  const config = buildAuctionConfig({ seriesId });
  const issuance = buildIssuanceConfig();
  const supplyPromis = BigInt(supplyIntex) * config.intexSize;
  const report = loadOrInitReport(args.seriesId, reportTitle(args.seriesId));

  await runStep(
    { report, network: outbeNet, phase: "auction", publicClient: outbe.publicClient },
    "Desis.sendAuctionStageClearing",
    `Signal clearing for series ${seriesId}; persist supply=${supplyIntex} Intex (${supplyPromis} Promis) and issuance config.`,
    async () => {
      // Prefunded float: OriginMessenger pays the LZ fee from its native balance.
      const txHash = (await desis.write.sendAuctionStageClearing(
        [seriesId, supplyPromis, issuance, "0x"],
      )) as `0x${string}`;
      const lz = await awaitLzDelivery({
        srcNetwork: outbeNet,
        dstNetwork: bnbNet,
        srcPublic: outbe.publicClient,
        dstPublic: bnb.publicClient,
        srcOApp: requireAddress(aO, "originMessenger", outbeNet) as Address,
        dstOApp: requireAddress(aB, "targetMessenger", bnbNet) as Address,
      });
      return {
        txHash,
        lz,
        notes: `bidPrice ${args.bidPrice} retained for relay-phase reveal payload.`,
      };
    },
  );
};

// --------------------------------------------------------------------------
// Phase 6: relay verification (BNB -> Outbe auto-relay)
// TargetMessenger._handleAuctionStageClearing automatically calls relayBidsToOutbe inside the
// clearing-stage handler — bids are bridged back to Outbe by the protocol, then OriginMessenger
// dispatches them to Desis and auto-fires clearAuction. This phase just waits for the cascade.
// --------------------------------------------------------------------------
const relayAction = async (args: CommonArgs) => {
  const outbeNet = args.outbeNetwork as DemoNetwork;
  const seriesId = Number(args.seriesId);
  const outbe = getRunner(outbeNet);
  const aO = resolveAddresses(outbeNet);
  const desis = contractAt(outbe, "Desis", requireAddress(aO, "desis", outbeNet) as Address);
  const report = loadOrInitReport(args.seriesId, reportTitle(args.seriesId));

  await runStep(
    { report, network: outbeNet, phase: "auction", publicClient: outbe.publicClient },
    "Await auto-relay BNB -> Outbe",
    `Wait for the bids batch auto-relayed by TargetMessenger after the clearing signal arrived.`,
    async () => {
      await assertCrossChainState({
        label: "Outbe Desis stage advanced past Revealing",
        read: async () => {
          const s = Number(await desis.read.getAuctionStage([seriesId]));
          const name = AUCTION_STAGE[s] ?? String(s);
          return s >= 3 ? "advanced" : `stuck(${name})`;
        },
        expected: "advanced",
        dstMessenger: requireAddress(aO, "originMessenger", outbeNet) as Address,
        dstPublic: outbe.publicClient,
      });
      const stage = Number(await desis.read.getAuctionStage([seriesId]));
      return {
        notes: `Desis stage after auto-relay: ${AUCTION_STAGE[stage] ?? stage} (BidsReceived if auto-clear deferred, Cleared if it ran inline).`,
      };
    },
  );
};

// --------------------------------------------------------------------------
// Phase 7: verify (BNB) — series created + minted on BNB IntexNFT1155 by OriginMessenger's auto-clear.
// --------------------------------------------------------------------------
const verifyAction = async (args: CommonArgs) => {
  const bnbNet = args.bnbNetwork as DemoNetwork;
  const seriesId = Number(args.seriesId);
  const bnb = getRunner(bnbNet);
  const aB = resolveAddresses(bnbNet);
  const intex = contractAt(bnb, "IntexNFT1155", requireAddress(aB, "intexNFT1155", bnbNet) as Address);
  const report = loadOrInitReport(args.seriesId, reportTitle(args.seriesId));

  await runStep(
    { report, network: bnbNet, phase: "auction", publicClient: bnb.publicClient },
    "Verify issuance on BNB",
    `Poll IntexNFT1155 series ${seriesId} on BNB until issued + check state.`,
    async () => {
      // The series lands on BNB after a cascade of Outbe -> BNB LZ sends fired inside
      // Desis.clearAuction (AuctionResult, IntexFactory.issue, RefundInstructions). Poll until
      // readData stops reverting with NonexistentToken, then assert the state.
      // IntexState enum (IIntexNFT1155): Issued=0, Qualified=1, Called=2.
      const stateName = (s: number) => ["Issued", "Qualified", "Called"][s] ?? `unknown(${s})`;
      await assertCrossChainState({
        label: "BNB IntexNFT1155 series exists",
        read: async () => {
          const data = (await intex.read.readData([seriesId])) as {
            issuedIntexCount: bigint;
            state: number;
          };
          return stateName(data.state);
        },
        expected: "Issued",
        dstMessenger: requireAddress(aB, "targetMessenger", bnbNet) as Address,
        dstPublic: bnb.publicClient,
      });
      const data = (await intex.read.readData([seriesId])) as {
        issuedIntexCount: bigint;
        state: number;
      };
      return {
        assertions: [
          assertState("issuedIntexCount > 0", "true", String(Number(data.issuedIntexCount) > 0)),
          assertState("series state", "Issued", stateName(data.state)),
        ],
        notes: `Run complete. Full evidence in reports/${args.seriesId}/report.md`,
      };
    },
  );
};

// --------------------------------------------------------------------------
// Full cycle: run all eight phases in order, gating on Enter between each
// --------------------------------------------------------------------------
interface AllArgs extends CommonArgs {
  quantity: string;
  bidPrice: string;
  supply: string;
  pause: string;
}

/** Block until the operator presses Enter — the manual gate between phases in the guided run. */
const waitForEnter = (prompt: string): Promise<void> =>
  new Promise((resolve) => {
    const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
    rl.question(prompt, () => {
      rl.close();
      resolve();
    });
  });

// Phases 6 and 7 wait for protocol-driven async cascades (auto-relay + auto-clearAuction +
// Outbe -> BNB issuance LZ sends), so they always run unattended after Phase 5 — pausing for Enter
// there would just stall the operator with nothing to confirm.
const AUTO_PHASES = new Set(["relay", "verify"]);

const allAction = async (args: AllArgs) => {
  const phases: { name: string; run: () => Promise<void> }[] = [
    { name: "start", run: () => startAction(args) },
    { name: "commit", run: () => commitAction(args) },
    { name: "reveal", run: () => revealAction(args) },
    { name: "reveal-bid", run: () => revealBidAction(args) },
    { name: "clearing", run: () => clearingAction(args) },
    { name: "relay", run: () => relayAction(args) },
    { name: "verify", run: () => verifyAction(args) },
  ];
  const interactive = args.pause !== "false";
  for (let i = 0; i < phases.length; i++) {
    const { name, run } = phases[i];
    console.log(`\n========== Phase ${i + 1}/${phases.length}: ${name} ==========`);
    await run();
    const next = phases[i + 1];
    if (next && interactive && !AUTO_PHASES.has(next.name)) {
      await waitForEnter(`\n✔ ${name} complete. Press Enter to run "${next.name}" (Ctrl+C to stop)… `);
    }
  }
  console.log(`\n✔ Full auction cycle complete. Report: reports/${args.seriesId}/report.md`);
};

// --------------------------------------------------------------------------
// Task registration
// --------------------------------------------------------------------------
const start = commonOpts(task("demo:auction:start", "Auction demo phase 1/7: start on Outbe -> BNB")).setAction(
  lazy(startAction),
);
const commit = commonOpts(task("demo:auction:commit", "Auction demo phase 2/7: commit a sealed bid on BNB"))
  .addOption(opt("quantity", "Bid quantity (Intex units)", "5"))
  .addOption(opt("bidPrice", "Bid price per Intex (payment-token minor units)", "60000000"))
  .setAction(lazy(commitAction));
const reveal = commonOpts(task("demo:auction:reveal", "Auction demo phase 3/7: open the reveal stage -> BNB")).setAction(
  lazy(revealAction),
);
const revealBid = commonOpts(task("demo:auction:reveal-bid", "Auction demo phase 4/7: reveal the committed bid on BNB"))
  .addOption(opt("quantity", "Bid quantity (must match the commit)", "5"))
  .addOption(opt("bidPrice", "Bid price (must match the commit)", "60000000"))
  .setAction(lazy(revealBidAction));
const clearing = commonOpts(
  task("demo:auction:clearing", "Auction demo phase 5/7: signal clearing + persist supply/issuance"),
)
  .addOption(opt("supply", "Issued supply in Intex units (Desis multiplies by intexSize for Promis)", "100"))
  .addOption(opt("bidPrice", "Bid price reference (used by relay-phase reveal payload)", "60000000"))
  .setAction(lazy(clearingAction));
const relay = commonOpts(task("demo:auction:relay", "Auction demo phase 6/7: wait for the auto-relayed bids batch (TargetMessenger fires it inside the clearing handler; OriginMessenger then auto-fires clearAuction)"))
  .setAction(lazy(relayAction));
const verify = commonOpts(task("demo:auction:verify", "Auction demo phase 7/7: verify series + mint on BNB")).setAction(
  lazy(verifyAction),
);
const all = commonOpts(
  task("demo:auction:all", "Auction demo: run all seven phases in order, pausing for Enter between each"),
)
  .addOption(opt("quantity", "Bid quantity (Intex units)", "5"))
  .addOption(opt("bidPrice", "Bid price per Intex (payment-token minor units)", "60000000"))
  .addOption(opt("supply", "Issued supply for clearing (Intex units)", "100"))
  .addOption(opt("pause", 'Pause for Enter between phases ("false" runs unattended)', "true"))
  .setAction(lazy(allAction));

export const auctionDemoTasks = [
  start.build(),
  commit.build(),
  reveal.build(),
  revealBid.build(),
  clearing.build(),
  relay.build(),
  verify.build(),
  all.build(),
];
