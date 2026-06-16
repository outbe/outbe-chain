// Auction Stage Management Tasks
// Individual Hardhat tasks for each auction lifecycle stage.

import { task } from "hardhat/config";
import {
  runAuctionClearingCore,
  runAuctionExecuteCore,
  runAuctionFlowCore,
  runAuctionRevealCore,
  runAuctionStartCore,
} from "../../scripts/auction/flow.js";
import { createAuctionRuntime } from "../../scripts/shared/runtime.js";
import { lazy, toOptional, parseBoolean } from "../../scripts/shared/taskUtils.js";
import type {
  AuctionFlowTaskArgs,
  AuctionStartTaskArgs,
  AuctionRevealTaskArgs,
  AuctionClearingTaskArgs,
  AuctionExecuteTaskArgs,
} from "../types.js";

// =============================================================================
// Helpers
// =============================================================================

function requireAddress(value: string | undefined, name: string): string {
  if (!value) {
    throw new Error(`Missing required parameter: --${name}`);
  }
  return value;
}

// =============================================================================
// Task Actions
// =============================================================================

const auctionFlowAction = async (args: AuctionFlowTaskArgs, hre: unknown) => {
  const intexAuctionContract = requireAddress(toOptional(args.intexAuctionContract), "auction-contract");
  const runtime = await createAuctionRuntime(hre, intexAuctionContract);

  await runAuctionFlowCore({
    runtime,
    series: toOptional(args.series),
    floor: toOptional(args.floor),
    commitEnd: toOptional(args.commitEnd),
    revealEnd: toOptional(args.revealEnd),
    issuanceEnd: toOptional(args.issuanceEnd),
    promisLoadMinor: toOptional(args.promisLoadMinor),
    costAmountMinor: toOptional(args.costAmountMinor),
    floorPriceMinor: toOptional(args.floorPriceMinor),
    minIntexBidQuantity: toOptional(args.minIntexBidQuantity),
    isGreenDay: args.isGreenDay,
    issuedIntexCount: toOptional(args.issuedIntexCount),
    clearingPrice: toOptional(args.clearingPrice),
    wonBidsCount: toOptional(args.wonBidsCount),
    skipStart: Boolean(args.skipStart),
    skipReveal: Boolean(args.skipReveal),
    skipClearing: Boolean(args.skipClearing),
    skipExecute: Boolean(args.skipExecute),
    finalizeEscrow: parseBoolean(args.finalizeEscrow, true),
    escrowAdapterAddress: toOptional(args.escrowAdapterContract),
    paymentTokenAddress: toOptional(args.paymentTokenContract),
  });
};

const auctionStartAction = async (args: AuctionStartTaskArgs, hre: unknown) => {
  const intexAuctionContract = requireAddress(toOptional(args.intexAuctionContract), "auction-contract");
  const runtime = await createAuctionRuntime(hre, intexAuctionContract);

  await runAuctionStartCore({
    runtime,
    series: toOptional(args.series),
    floor: toOptional(args.floor),
    commitEnd: toOptional(args.commitEnd),
    revealEnd: toOptional(args.revealEnd),
    issuanceEnd: toOptional(args.issuanceEnd),
    promisLoadMinor: toOptional(args.promisLoadMinor),
    costAmountMinor: toOptional(args.costAmountMinor),
    floorPriceMinor: toOptional(args.floorPriceMinor),
    minIntexBidQuantity: toOptional(args.minIntexBidQuantity),
  });
};

const auctionRevealAction = async (args: AuctionRevealTaskArgs, hre: unknown) => {
  const intexAuctionContract = requireAddress(toOptional(args.intexAuctionContract), "auction-contract");
  const runtime = await createAuctionRuntime(hre, intexAuctionContract);

  await runAuctionRevealCore({
    runtime,
    series: toOptional(args.series),
    isGreenDay: args.isGreenDay,
  });
};

const auctionClearingAction = async (args: AuctionClearingTaskArgs, hre: unknown) => {
  const intexAuctionContract = requireAddress(toOptional(args.intexAuctionContract), "auction-contract");
  const runtime = await createAuctionRuntime(hre, intexAuctionContract);

  await runAuctionClearingCore({
    runtime,
    series: toOptional(args.series),
  });
};

const auctionExecuteAction = async (args: AuctionExecuteTaskArgs, hre: unknown) => {
  const intexAuctionContract = requireAddress(toOptional(args.intexAuctionContract), "auction-contract");
  const runtime = await createAuctionRuntime(hre, intexAuctionContract);

  await runAuctionExecuteCore({
    runtime,
    series: toOptional(args.series),
    issuedIntexCount: toOptional(args.issuedIntexCount),
    clearingPrice: toOptional(args.clearingPrice),
    wonBidsCount: toOptional(args.wonBidsCount),
    finalizeEscrow: parseBoolean(args.finalizeEscrow, true),
    escrowAdapterAddress: toOptional(args.escrowAdapterContract),
    paymentTokenAddress: toOptional(args.paymentTokenContract),
  });
};

// =============================================================================
// Task Definitions
// =============================================================================

const auctionFlow = task("auction-flow", "Run full Auction flow")
  .addOption({ name: "intexAuctionContract", description: "Auction contract address (required)", defaultValue: "" })
  .addOption({ name: "series", description: "Series in yyyymmdd format", defaultValue: "today" })
  .addOption({ name: "floor", description: "Bid floor price", defaultValue: "80000000" })
  .addOption({ name: "commitEnd", description: "Commit stage end timestamp (UNIX)", defaultValue: "" })
  .addOption({ name: "revealEnd", description: "Reveal stage end timestamp (UNIX)", defaultValue: "" })
  .addOption({ name: "issuanceEnd", description: "Issuance stage end timestamp (UNIX)", defaultValue: "" })
  .addOption({ name: "promisLoadMinor", description: "Promis load", defaultValue: "1000000" })
  .addOption({ name: "costAmountMinor", description: "Cost amount", defaultValue: "1000000000" })
  .addOption({ name: "floorPriceMinor", description: "Floor price", defaultValue: "" })
  .addOption({ name: "minIntexBidQuantity", description: "Minimum bid quantity", defaultValue: "12" })
  .addOption({ name: "isGreenDay", description: "Is green day (true/false)", defaultValue: "true" })
  .addOption({ name: "issuedIntexCount", description: "Override issued Intex count", defaultValue: "" })
  .addOption({ name: "clearingPrice", description: "Override clearing price", defaultValue: "" })
  .addOption({ name: "wonBidsCount", description: "Override won bids count", defaultValue: "" })
  .addFlag({ name: "skipStart", description: "Skip auction start" })
  .addFlag({ name: "skipReveal", description: "Skip reveal stage" })
  .addFlag({ name: "skipClearing", description: "Skip clearing stage" })
  .addFlag({ name: "skipExecute", description: "Skip execute clearing" })
  .addOption({ name: "finalizeEscrow", description: "Finalize escrow (true/false)", defaultValue: "true" })
  .addOption({ name: "escrowAdapterContract", description: "EscrowAdapter contract address", defaultValue: "" })
  .addOption({ name: "paymentTokenContract", description: "Stable token contract address", defaultValue: "" })
  .setAction(lazy(auctionFlowAction));

const auctionStart = task("auction-start", "Start a new auction")
  .addOption({ name: "intexAuctionContract", description: "Auction contract address (required)", defaultValue: "" })
  .addOption({ name: "series", description: "Series in yyyymmdd format", defaultValue: "" })
  .addOption({ name: "floor", description: "Bid floor price", defaultValue: "80000000" })
  .addOption({ name: "commitEnd", description: "Commit stage end timestamp (UNIX)", defaultValue: "" })
  .addOption({ name: "revealEnd", description: "Reveal stage end timestamp (UNIX)", defaultValue: "" })
  .addOption({ name: "issuanceEnd", description: "Issuance stage end timestamp (UNIX)", defaultValue: "" })
  .addOption({ name: "promisLoadMinor", description: "Promis load", defaultValue: "1000000" })
  .addOption({ name: "costAmountMinor", description: "Cost amount", defaultValue: "1000000000" })
  .addOption({ name: "floorPriceMinor", description: "Floor price", defaultValue: "" })
  .addOption({ name: "minIntexBidQuantity", description: "Minimum bid quantity", defaultValue: "12" })
  .setAction(lazy(auctionStartAction));

const auctionReveal = task("auction-reveal", "Start reveal stage")
  .addOption({ name: "intexAuctionContract", description: "Auction contract address (required)", defaultValue: "" })
  .addOption({ name: "series", description: "Series in yyyymmdd format", defaultValue: "" })
  .addOption({ name: "isGreenDay", description: "Is green day (true/false)", defaultValue: "true" })
  .setAction(lazy(auctionRevealAction));

const auctionClearing = task("auction-clearing", "Start clearing stage")
  .addOption({ name: "intexAuctionContract", description: "Auction contract address (required)", defaultValue: "" })
  .addOption({ name: "series", description: "Series in yyyymmdd format", defaultValue: "" })
  .setAction(lazy(auctionClearingAction));

const auctionExecute = task("auction-execute", "Execute auction clearing")
  .addOption({ name: "intexAuctionContract", description: "Auction contract address (required)", defaultValue: "" })
  .addOption({ name: "series", description: "Series in yyyymmdd format", defaultValue: "" })
  .addOption({ name: "issuedIntexCount", description: "Override issued Intex count", defaultValue: "" })
  .addOption({ name: "clearingPrice", description: "Override clearing price", defaultValue: "" })
  .addOption({ name: "wonBidsCount", description: "Override won bids count", defaultValue: "" })
  .addOption({ name: "finalizeEscrow", description: "Finalize escrow (true/false)", defaultValue: "true" })
  .addOption({ name: "escrowAdapterContract", description: "EscrowAdapter contract address", defaultValue: "" })
  .addOption({ name: "paymentTokenContract", description: "Stable token contract address", defaultValue: "" })
  .setAction(lazy(auctionExecuteAction));

// =============================================================================
// Export
// =============================================================================

export const auctionStageTasks = [
  auctionFlow.build(),
  auctionStart.build(),
  auctionReveal.build(),
  auctionClearing.build(),
  auctionExecute.build(),
];
