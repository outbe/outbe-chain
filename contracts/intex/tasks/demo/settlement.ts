// E2 / E3 — settlement demo runbooks (QC-1261).
//
// E2 (Qualified, "settle by will"): mark the series Qualified on Outbe -> bridge MARK_QUALIFIED to
// BNB -> the holder settles. E3 (Called, "settle from call"): mark Called -> bridge MARK_CALLED ->
// settle. The mark phase is cross-chain (LZ proof); the settle phase runs on Outbe via
// IntexSettlement. Reuses the E0 harness + the E1 orchestrator runtime.

import { task } from "hardhat/config";
import { type Address } from "viem";

import { loadOrInitReport } from "../../scripts/demo/harness/report.js";
import { runStep, assertState } from "../../scripts/demo/harness/runner.js";
import { awaitLzDelivery } from "../../scripts/demo/harness/lz.js";
import { resolveAddresses, requireAddress, type DemoNetwork } from "../../scripts/demo/harness/config.js";
import { getRunner, contractAt, type MessagingFee } from "../../scripts/demo/auction.js";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const lazy = (fn: (args: any, hre: any) => Promise<void>) => async () => ({ default: fn });
const opt = (name: string, description: string, defaultValue = "") => ({ name, description, defaultValue });

const title = (seriesId: string): string => `Settlement demo — series ${seriesId}`;

interface MarkArgs {
  seriesId: string;
  outbeNetwork: string;
  bnbNetwork: string;
}

/** Shared mark-phase: IntexFactory.markSeries{Qualified,Called} on Outbe -> LZ -> assert BNB state. */
async function runMark(args: MarkArgs, kind: "Qualified" | "Called", expectedState: string) {
  const outbeNet = args.outbeNetwork as DemoNetwork;
  const bnbNet = args.bnbNetwork as DemoNetwork;
  const seriesId = Number(args.seriesId);
  const outbe = getRunner(outbeNet);
  const bnb = getRunner(bnbNet);
  const aO = resolveAddresses(outbeNet);
  const aB = resolveAddresses(bnbNet);
  const factory = contractAt(outbe, "IntexFactory", requireAddress(aO, "intexFactory", outbeNet) as Address);
  const origin = contractAt(outbe, "OriginMessenger", requireAddress(aO, "originMessenger", outbeNet) as Address);
  const intex = contractAt(bnb, "IntexNFT1155", requireAddress(aB, "intexNFT1155", bnbNet) as Address);
  const report = loadOrInitReport(args.seriesId, title(args.seriesId));
  const fn = kind === "Qualified" ? "markSeriesQualified" : "markSeriesCalled";
  const quoteFn = kind === "Qualified" ? "quoteSendMarkQualified" : "quoteSendMarkCalled";

  await runStep(
    { report, network: outbeNet, phase: `settlement-${kind.toLowerCase()}`, publicClient: outbe.publicClient },
    `IntexFactory.${fn}`,
    `Mark series ${seriesId} ${kind} on Outbe and bridge MARK_${kind.toUpperCase()} to BNB.`,
    async () => {
      const fee = (await origin.read[quoteFn]([seriesId, "0x", false])) as MessagingFee;
      const txHash = (await factory.write[fn]([seriesId, "0x"], { value: fee.nativeFee })) as `0x${string}`;
      const lz = await awaitLzDelivery({
        srcNetwork: outbeNet,
        dstNetwork: bnbNet,
        srcPublic: outbe.publicClient,
        dstPublic: bnb.publicClient,
        srcOApp: requireAddress(aO, "originMessenger", outbeNet) as Address,
        dstOApp: requireAddress(aB, "targetMessenger", bnbNet) as Address,
      });
      const data = (await intex.read.readData([seriesId])) as { state: number };
      return { txHash, lz, assertions: [assertState("BNB series state", expectedState, String(data.state))] };
    },
  );
  console.log(`\nReport: reports/${args.seriesId}/report.md`);
}

interface SettleArgs {
  seriesId: string;
  outbeNetwork: string;
  holder: string;
  amount: string;
  settler: string;
}

/** Settle phase (Outbe): authorize a settler, then IntexSettlement.settle the holder's amount. */
const settleAction = async (args: SettleArgs) => {
  const outbeNet = args.outbeNetwork as DemoNetwork;
  const seriesId = Number(args.seriesId);
  const outbe = getRunner(outbeNet);
  const aO = resolveAddresses(outbeNet);
  const settlement = contractAt(outbe, "IntexSettlement", requireAddress(aO, "intexSettlement", outbeNet) as Address);
  const holder = (args.holder || outbe.account.address) as Address;
  const settler = (args.settler || outbe.account.address) as Address;
  const amount = BigInt(args.amount);
  const report = loadOrInitReport(args.seriesId, title(args.seriesId));
  const ctx = { report, network: outbeNet, phase: "settlement", publicClient: outbe.publicClient };

  await runStep(ctx, "IntexSettlement.authorizeSettler", `Authorize ${settler} as settler for series ${seriesId}.`, async () => {
    const txHash = (await settlement.write.authorizeSettler([seriesId, settler])) as `0x${string}`;
    return { txHash };
  });

  await runStep(ctx, "IntexSettlement.settle", `Settle ${amount} for holder ${holder} on series ${seriesId}.`, async () => {
    const txHash = (await settlement.write.settle([seriesId, holder, amount])) as `0x${string}`;
    return { txHash, notes: `settled amount ${amount} for ${holder}` };
  });
  console.log(`\nReport: reports/${args.seriesId}/report.md`);
};

// --- Tasks ---
const markQualified = task("demo:settlement:mark-qualified", "Settlement E2: mark series Qualified -> BNB")
  .addOption(opt("seriesId", "Series id (also the report run id)"))
  .addOption(opt("outbeNetwork", "Outbe network", "outbeTestnetNew"))
  .addOption(opt("bnbNetwork", "BNB network", "bscTestnet"))
  .setAction(lazy((args: MarkArgs) => runMark(args, "Qualified", "Qualified(2)")));

const markCalled = task("demo:settlement:mark-called", "Settlement E3: mark series Called -> BNB")
  .addOption(opt("seriesId", "Series id (also the report run id)"))
  .addOption(opt("outbeNetwork", "Outbe network", "outbeTestnetNew"))
  .addOption(opt("bnbNetwork", "BNB network", "bscTestnet"))
  .setAction(lazy((args: MarkArgs) => runMark(args, "Called", "Called(3)")));

const settle = task("demo:settlement:settle", "Settlement E2/E3: authorize settler + IntexSettlement.settle")
  .addOption(opt("seriesId", "Series id (also the report run id)"))
  .addOption(opt("outbeNetwork", "Outbe network", "outbeTestnetNew"))
  .addOption(opt("holder", "Intex holder to settle (default: the runner)"))
  .addOption(opt("amount", "Amount to settle"))
  .addOption(opt("settler", "Settler to authorize (default: the runner)"))
  .setAction(lazy(settleAction));

export const settlementDemoTasks = [markQualified.build(), markCalled.build(), settle.build()];
