// Cross-chain Auction Task
// Interactive auction flow for Telosis (Outbe-side auction lifecycle).
// Controls the auction lifecycle from Outbe side, where the bridge communicates with BNB.

import { task } from "hardhat/config";
import {
  createOutbeRuntime,
  createAndStartAuction,
  sendRevealStage,
  sendClearingStage,
  clearAuction,
  sendIssuanceInstructions,
  sendRefundInstructions,
  getAuctionInfo,
  getAuctionBids,
  fundBnbBridgeAdapter,
  waitForLzDelivery,
} from "../../scripts/auction/crosschainFlow.js";
import { runCommitBids, runRevealBids } from "../../scripts/auction/bidders.js";
import { createBidderRuntime } from "../../scripts/shared/runtime.js";
import { normalizeSeries, resolveSeriesId } from "../../scripts/shared/auctionId.js";
import { loadWallets, DEFAULT_WALLETS_PATH, DEFAULT_COMMITS_PATH } from "../../scripts/shared/wallets.js";
import { promptForContinue } from "../../scripts/shared/prompt.js";
import { lazy, toOptional, parseBoolean, getNetworkName } from "../../scripts/shared/taskUtils.js";
import type { OutbeMockInteractiveTaskArgs } from "../types.js";
// =============================================================================
// Helpers
// =============================================================================

function requireAddress(value: string | undefined, name: string): `0x${string}` {
  if (!value) {
    throw new Error(`Missing required parameter: --${name}`);
  }
  return value as `0x${string}`;
}

function toOptionalBigInt(input: string | undefined): bigint | undefined {
  if (!input) return undefined;
  const trimmed = input.trim();
  return trimmed === "" ? undefined : BigInt(trimmed);
}

/** Parse "min,max" string into [number, number] tuple */
function parseRange(input: string | undefined): [number, number] | undefined {
  if (!input) return undefined;
  const parts = input.split(",").map((s) => Number(s.trim()));
  if (parts.length !== 2 || parts.some(isNaN)) return undefined;
  return [parts[0], parts[1]];
}

/** Parse "min,max" price string into [string, string] tuple */
function parsePriceRange(input: string | undefined): [string, string] | undefined {
  if (!input) return undefined;
  const parts = input.split(",").map((s) => s.trim());
  if (parts.length !== 2) return undefined;
  return [parts[0], parts[1]];
}

// =============================================================================
// Task Action
// =============================================================================

// Interactive Outbe auction flow:
//
// 1. Create and start auction on Telosis (+ BNB bridge message)
// 2. PAUSE - Wait for commits on BNB (optionally run bidders script)
// 3. Send reveal stage to BNB
// 4. PAUSE - Wait for reveals on BNB (optionally run bidders script)
// 5. Send clearing stage to BNB
// 6. PAUSE - Wait for bids to arrive on Outbe
// 7. Clear auction on Telosis
// 8. Send issuance instructions to BNB
// 9. Send refund instructions to BNB
const outbeInteractiveAction = async (args: OutbeMockInteractiveTaskArgs, hre: unknown) => {
  const desisContract = requireAddress(toOptional(args.desisContract), "telosis-contract");

  const bnbAuctionContract = toOptional(args.bnbAuctionContract) as `0x${string}` | undefined;
  const bnbEscrowAdapterContract = toOptional(args.bnbEscrowAdapterContract) as `0x${string}` | undefined;
  const bnbPaymentTokenContract = toOptional(args.bnbPaymentTokenContract) as `0x${string}` | undefined;
  const targetMessengerContract = toOptional(args.targetMessengerContract as string | undefined) as `0x${string}` | undefined;

  const withBidders = parseBoolean(args.withBidders, false);
  const isGreenDay = parseBoolean(args.isGreenDay, true);

  const walletsPath = args.wallets || DEFAULT_WALLETS_PATH;
  const commitsFile = args.commitsFile || DEFAULT_COMMITS_PATH;

  const quantityRange = parseRange(toOptional(args.quantityRange)) as [number, number] | undefined;
  const priceRange = parsePriceRange(toOptional(args.priceRange));

  const series = normalizeSeries(toOptional(args.series));
  const seriesId = resolveSeriesId(series);
  const supply = toOptionalBigInt(toOptional(args.supply));
  const msgValue = toOptionalBigInt(toOptional(args.msgValue));

  const runtime = await createOutbeRuntime(hre, {
    telosisAddress: desisContract,
  });

  const biddersNetwork = (toOptional(args.bnbBiddersNetwork) || "bscTestnet") as string;

  console.log("\n=== Outbe Interactive Auction Flow ===");
  console.log({
    desisContract,
    series,
    seriesId,
    withBidders,
    biddersNetwork: withBidders ? biddersNetwork : "(n/a)",
    supply: supply ? supply.toString() : "(not set — required for clearing)",
  });

  // Step 1: Create and start auction on Telosis
  console.log("\n[1/9] Creating and starting auction on Telosis...");
  await createAndStartAuction(runtime, {
    series,
    floor: toOptional(args.floor),
    clearingTimestamp: toOptional(args.clearingTimestamp),
    intexSize: toOptional(args.intexSize),
    intexStrikePrice: toOptional(args.intexStrikePrice),
    minIntexBidQuantity: toOptional(args.minIntexBidQuantity),
    msgValue,
  });
  console.log("Auction created and started on BNB");

  // Step 2: Wait for commits on BNB
  if (withBidders && bnbAuctionContract) {
    console.log("\n[2/9] Running bidders commit script on BNB...");
    const bidderRuntime = await createBidderRuntime(hre, bnbAuctionContract, { networkForBidders: biddersNetwork });
    const wallets = loadWallets(walletsPath);
    await runCommitBids(bidderRuntime, {
      seriesId,
      wallets,
      commitsFile,
      quantityRange,
      priceRange,
    });
    console.log("Script commits completed on BNB");
  } else {
    console.log("\n[2/9] PAUSE: Waiting for commits on BNB...");
    console.log(`   Series ID: ${seriesId}`);
  }

  await promptForContinue("Press Enter to send REVEAL stage to BNB...");

  // Step 3: Send reveal stage to BNB
  console.log("\n[3/9] Sending reveal stage to BNB...");
  await sendRevealStage(runtime, { seriesId, isGreenDay, msgValue });
  console.log("Reveal stage sent to BNB (waiting for LZ delivery...)");

  // Step 4: Wait for reveals on BNB
  if (withBidders && bnbAuctionContract && bnbEscrowAdapterContract && bnbPaymentTokenContract) {
    console.log("\n[4/9] Running bidders reveal script on BNB...");
    const bidderRuntime = await createBidderRuntime(hre, bnbAuctionContract, { networkForBidders: biddersNetwork });
    const wallets = loadWallets(walletsPath);
    await runRevealBids(bidderRuntime, {
      seriesId,
      wallets,
      commitsFile,
      escrowAdapterAddress: bnbEscrowAdapterContract,
      paymentTokenAddress: bnbPaymentTokenContract,
    });
    console.log("Script reveals completed on BNB");
  } else {
    console.log("\n[4/9] PAUSE: Waiting for reveals on BNB...");
    console.log(`   Series ID: ${seriesId}`);
  }

  await promptForContinue("Press Enter to send CLEARING stage to BNB...");

  if (targetMessengerContract) {
    console.log("\n[pre-5] Ensuring TargetMessenger has native balance for return LZ message...");
    await fundBnbBridgeAdapter({
      adapterAddress: targetMessengerContract,
      networkId: biddersNetwork,
    });
  }

  // Step 5: Send clearing stage to BNB
  console.log("\n[5/9] Sending clearing stage to BNB...");
  await sendClearingStage(runtime, { seriesId, msgValue });
  console.log("Clearing stage sent to BNB");

  // Step 6: Wait for bids to arrive on Outbe (LZ delivery from BNB -> Outbe)
  console.log("\n[6/9] Waiting for bids to arrive on Outbe (LayerZero delivery)...");
  const POLL_MS = 5_000;
  const MAX_POLLS = 24;
  let info = await getAuctionInfo(runtime, seriesId);
  for (let i = 0; i < MAX_POLLS && info.bidsCount === 0n; i++) {
    if (i === 0) console.log(`   Polling getBidsCount every ${POLL_MS / 1000}s...`);
    await new Promise((r) => setTimeout(r, POLL_MS));
    info = await getAuctionInfo(runtime, seriesId);
  }
  console.log(`   Bids received: ${info.bidsCount}`);
  console.log(`   Auction stage: ${info.stageName}`);

  if (info.bidsCount === 0n) {
    console.warn("[warn] No bids received after polling! Clearing may result in empty auction.");
    await promptForContinue("Press Enter to continue anyway, or Ctrl+C to abort...");
  }

  // Step 7: Clear auction on Telosis
  console.log("\n[7/9] Clearing auction on Telosis...");
  const clearingResult = await clearAuction(runtime, { seriesId, supply, msgValue });
  console.log("Auction cleared");
  console.log(`   Issued INTEX: ${clearingResult.issuedIntexCount}`);
  console.log(`   Clearing price: ${clearingResult.clearingPrice}`);
  console.log(`   Winners: ${clearingResult.winners.length}`);

  const bridgeAdapterAddress = await runtime.telosis.read.bridgeAdapter();
  const outbeNetworkId = getNetworkName(hre, "outbeDevnet");
  const lzWaitArgs = { bridgeAdapterAddress, bscNetworkId: biddersNetwork, outbeNetworkId };

  console.log("\n[7.5/9] Waiting for auction result delivery on BSC...");
  await waitForLzDelivery(runtime, lzWaitArgs);

  // Step 8: Send issuance instructions to BNB
  console.log("\n[8/9] Sending issuance instructions to BNB...");
  await sendIssuanceInstructions(runtime, { seriesId, msgValue });
  console.log("Issuance instructions sent to BNB");

  console.log("\n[8.5/9] Waiting for issuance instructions delivery on BSC...");
  await waitForLzDelivery(runtime, lzWaitArgs);

  // Step 9: Send refund instructions to BNB
  console.log("\n[9/9] Sending refund instructions to BNB...");
  await sendRefundInstructions(runtime, { seriesId, msgValue });
  console.log("Refund instructions sent to BNB");

  console.log("\n[9.5/9] Waiting for refund instructions delivery on BSC...");
  await waitForLzDelivery(runtime, lzWaitArgs);

  // Final summary
  console.log("\n=== Outbe Interactive Flow Completed ===");
  console.log({
    seriesId,
    issuedIntexCount: clearingResult.issuedIntexCount.toString(),
    clearingPrice: clearingResult.clearingPrice.toString(),
    winnersCount: clearingResult.winners.length,
  });
};

// =============================================================================
// Individual Step Tasks
// =============================================================================

// Create and start auction
const createAndStartAction = async (args: OutbeMockInteractiveTaskArgs, hre: unknown) => {
  const desisContract = requireAddress(toOptional(args.desisContract), "telosis-contract");

  const runtime = await createOutbeRuntime(hre, {
    telosisAddress: desisContract,
  });

  const series = normalizeSeries(toOptional(args.series));
  const msgValue = toOptionalBigInt(toOptional(args.msgValue));

  const { seriesId } = await createAndStartAuction(runtime, {
    series,
    floor: toOptional(args.floor),
    clearingTimestamp: toOptional(args.clearingTimestamp),
    intexSize: toOptional(args.intexSize),
    intexStrikePrice: toOptional(args.intexStrikePrice),
    minIntexBidQuantity: toOptional(args.minIntexBidQuantity),
    msgValue,
  });

  console.log(`Auction created and started: ${seriesId}`);
};

// Send reveal stage only
const sendRevealAction = async (args: OutbeMockInteractiveTaskArgs, hre: unknown) => {
  const desisContract = requireAddress(toOptional(args.desisContract), "telosis-contract");

  const runtime = await createOutbeRuntime(hre, {
    telosisAddress: desisContract,
  });

  const seriesId = resolveSeriesId(toOptional(args.series));
  const isGreenDay = parseBoolean(args.isGreenDay, true);
  const msgValue = toOptionalBigInt(toOptional(args.msgValue));

  await sendRevealStage(runtime, { seriesId, isGreenDay, msgValue });
  console.log("Reveal stage sent");
};

// Send clearing stage only
const sendClearingAction = async (args: OutbeMockInteractiveTaskArgs, hre: unknown) => {
  const desisContract = requireAddress(toOptional(args.desisContract), "telosis-contract");

  const runtime = await createOutbeRuntime(hre, {
    telosisAddress: desisContract,
  });

  const seriesId = resolveSeriesId(toOptional(args.series));
  const msgValue = toOptionalBigInt(toOptional(args.msgValue));

  await sendClearingStage(runtime, { seriesId, msgValue });
  console.log("Clearing stage sent");
};

// Clear auction only
const clearAuctionAction = async (args: OutbeMockInteractiveTaskArgs, hre: unknown) => {
  const desisContract = requireAddress(toOptional(args.desisContract), "telosis-contract");

  const runtime = await createOutbeRuntime(hre, {
    telosisAddress: desisContract,
  });

  const seriesId = resolveSeriesId(toOptional(args.series));
  const supply = toOptionalBigInt(toOptional(args.supply));
  const msgValue = toOptionalBigInt(toOptional(args.msgValue));

  const result = await clearAuction(runtime, { seriesId, supply, msgValue });
  console.log("Auction cleared:", {
    issuedIntexCount: result.issuedIntexCount.toString(),
    clearingPrice: result.clearingPrice.toString(),
    winnersCount: result.winners.length,
  });
};

// Check auction status
const checkAuctionAction = async (args: OutbeMockInteractiveTaskArgs, hre: unknown) => {
  const desisContract = requireAddress(toOptional(args.desisContract), "telosis-contract");

  const runtime = await createOutbeRuntime(hre, {
    telosisAddress: desisContract,
  });

  const seriesId = resolveSeriesId(toOptional(args.series));

  const info = await getAuctionInfo(runtime, seriesId);
  console.log("Auction info:", {
    seriesId,
    stage: info.stageName,
    bidsCount: info.bidsCount.toString(),
    config: {
      minIntexBidPrice: info.config.minIntexBidPrice.toString(),
      intexSize: info.config.intexSize.toString(),
      intexStrikePrice: info.config.intexStrikePrice.toString(),
      coenPriceFloor: info.config.coenPriceFloor.toString(),
      minIntexBidQuantity: info.config.minIntexBidQuantity,
    },
  });

  if (info.bidsCount > 0n) {
    const bids = await getAuctionBids(runtime, seriesId);
    console.log("Bids:");
    for (const bid of bids) {
      console.log(`  - ${bid.bidderAddress}: qty=${bid.intexQuantity}, price=${bid.intexBidPrice}`);
    }
  }
};

// =============================================================================
// Task Definitions
// =============================================================================

const outbeInteractive = task("outbe-interactive", "Interactive Outbe auction flow")
  .addOption({ name: "desisContract", description: "Telosis contract address (required)", defaultValue: "" })
  .addOption({ name: "series", description: "Series in yyyymmdd format", defaultValue: "" })
  .addOption({ name: "floor", description: "Bid floor price", defaultValue: "80000000" })
  .addOption({ name: "clearingTimestamp", description: "Clearing timestamp (UNIX)", defaultValue: "" })
  .addOption({ name: "intexSize", description: "Intex size (18 decimals)", defaultValue: "" })
  .addOption({ name: "intexStrikePrice", description: "Intex strike price", defaultValue: "1000000000" })
  .addOption({ name: "minIntexBidQuantity", description: "Minimum bid quantity", defaultValue: "5" })
  .addOption({ name: "supply", description: "INTEX supply (mandatory, must be > 0)", defaultValue: "" })
  .addOption({ name: "msgValue", description: "Native token value for LZ fees (wei)", defaultValue: "" })
  .addOption({ name: "isGreenDay", description: "Is green day (true/false)", defaultValue: "true" })
  .addOption({ name: "withBidders", description: "Run bidders scripts automatically", defaultValue: "false" })
  .addOption({ name: "wallets", description: "Path to wallets JSON", defaultValue: DEFAULT_WALLETS_PATH })
  .addOption({ name: "commitsFile", description: "Path to commits JSON", defaultValue: DEFAULT_COMMITS_PATH })
  .addOption({ name: "bnbAuctionContract", description: "BNB Auction contract address (for bidders)", defaultValue: "" })
  .addOption({ name: "bnbEscrowAdapterContract", description: "BNB EscrowAdapter contract address (for bidders)", defaultValue: "" })
  .addOption({ name: "bnbPaymentTokenContract", description: "BNB payment token contract address (for bidders)", defaultValue: "" })
  .addOption({ name: "targetMessengerContract", description: "TargetMessenger contract address (for pre-funding)", defaultValue: "" })
  .addOption({ name: "bnbBiddersNetwork", description: "Network for bidders (where Auction lives, e.g. bscTestnet)", defaultValue: "bscTestnet" })
  .addOption({ name: "quantityRange", description: "Bidder quantity range (min,max)", defaultValue: "" })
  .addOption({ name: "priceRange", description: "Bidder price range in USDT (min,max)", defaultValue: "" })
  .setAction(lazy(outbeInteractiveAction));

const outbeCreateAndStart = task("outbe-create-and-start", "Create and start auction on Telosis")
  .addOption({ name: "desisContract", description: "Telosis contract address (required)", defaultValue: "" })
  .addOption({ name: "series", description: "Series in yyyymmdd format", defaultValue: "" })
  .addOption({ name: "floor", description: "Bid floor price", defaultValue: "80000000" })
  .addOption({ name: "clearingTimestamp", description: "Clearing timestamp (UNIX)", defaultValue: "" })
  .addOption({ name: "intexSize", description: "Intex size (18 decimals)", defaultValue: "" })
  .addOption({ name: "intexStrikePrice", description: "Intex strike price", defaultValue: "1000000000" })
  .addOption({ name: "minIntexBidQuantity", description: "Minimum bid quantity", defaultValue: "5" })
  .addOption({ name: "msgValue", description: "Native token value for LZ fees (wei)", defaultValue: "" })
  .setAction(lazy(createAndStartAction));

const outbeSendReveal = task("outbe-send-reveal", "Send reveal stage to BNB")
  .addOption({ name: "desisContract", description: "Telosis contract address (required)", defaultValue: "" })
  .addOption({ name: "series", description: "Series in yyyymmdd format", defaultValue: "" })
  .addOption({ name: "isGreenDay", description: "Is green day (true/false)", defaultValue: "true" })
  .addOption({ name: "msgValue", description: "Native token value for LZ fees (wei)", defaultValue: "" })
  .setAction(lazy(sendRevealAction));

const outbeSendClearing = task("outbe-send-clearing", "Send clearing stage to BNB")
  .addOption({ name: "desisContract", description: "Telosis contract address (required)", defaultValue: "" })
  .addOption({ name: "series", description: "Series in yyyymmdd format", defaultValue: "" })
  .addOption({ name: "msgValue", description: "Native token value for LZ fees (wei)", defaultValue: "" })
  .setAction(lazy(sendClearingAction));

const outbeClearAuction = task("outbe-clear-auction", "Clear auction on Telosis")
  .addOption({ name: "desisContract", description: "Telosis contract address (required)", defaultValue: "" })
  .addOption({ name: "series", description: "Series in yyyymmdd format", defaultValue: "" })
  .addOption({ name: "supply", description: "INTEX supply (mandatory, must be > 0)", defaultValue: "" })
  .addOption({ name: "msgValue", description: "Native token value for LZ fees (wei)", defaultValue: "" })
  .setAction(lazy(clearAuctionAction));

const outbeCheckAuction = task("outbe-check-auction", "Check auction status on Telosis")
  .addOption({ name: "desisContract", description: "Telosis contract address (required)", defaultValue: "" })
  .addOption({ name: "series", description: "Series in yyyymmdd format", defaultValue: "" })
  .setAction(lazy(checkAuctionAction));

// =============================================================================
// Export
// =============================================================================

export const crosschainTasks = [
  outbeInteractive.build(),
  outbeCreateAndStart.build(),
  outbeSendReveal.build(),
  outbeSendClearing.build(),
  outbeClearAuction.build(),
  outbeCheckAuction.build(),
];
