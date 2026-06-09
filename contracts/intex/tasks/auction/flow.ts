// Auction Flow Task
// Full auction flow: start → bidders → pauses for manual bids → clearing → issuance.

import { task } from "hardhat/config";
import {
  runAuctionStartCore,
  runAuctionRevealCore,
  runAuctionClearingCore,
  runAuctionExecuteCore,
} from "../../scripts/auction/flow.js";
import { runCommitBids, runRevealBids } from "../../scripts/auction/bidders.js";
import { runIntex1155IssuanceCore } from "../../scripts/intex/issuance.js";
import {
  createAuctionRuntime,
  createBidderRuntime,
  createIntex1155IssuanceRuntime,
} from "../../scripts/shared/runtime.js";
import { normalizeSeries, resolveSeriesId } from "../../scripts/shared/auctionId.js";
import { loadWallets, DEFAULT_WALLETS_PATH, DEFAULT_COMMITS_PATH } from "../../scripts/shared/wallets.js";
import { promptForContinue } from "../../scripts/shared/prompt.js";
import { lazy, toOptional, parseBoolean, parseFloor } from "../../scripts/shared/taskUtils.js";
import type { AuctionInteractiveTaskArgs } from "../types.js";

// =============================================================================
// Helpers
// =============================================================================

function requireAddress(value: string | undefined, name: string): `0x${string}` {
  if (!value) {
    throw new Error(`Missing required parameter: --${name}`);
  }
  return value as `0x${string}`;
}

function toOptionalAddress(value?: string): `0x${string}` | undefined {
  return value ? (value as `0x${string}`) : undefined;
}

// =============================================================================
// Task Action
// =============================================================================

/**
 * Interactive auction flow:
 * 1. Start auction
 * 2. Script commits bids
 * 3. PAUSE - User commits manually
 * 4. Start reveal stage
 * 5. Script reveals bids
 * 6. PAUSE - User reveals manually
 * 7. Start clearing stage
 * 8. Execute clearing
 * 9. IntexNFT1155 issuance
 */
const auctionInteractiveAction = async (args: AuctionInteractiveTaskArgs, hre: unknown) => {
  // Resolve contract addresses (all required)
  const intexAuctionContract = requireAddress(toOptional(args.intexAuctionContract), "auction-contract");
  const escrowAdapterContract = requireAddress(toOptional(args.escrowAdapterContract), "escrow-adapter-contract");
  const paymentTokenContract = requireAddress(toOptional(args.paymentTokenContract), "payment-token-contract");
  const intexContract = requireAddress(toOptional(args.intexContract), "intex-contract");

  const auctionRuntime = await createAuctionRuntime(hre, intexAuctionContract);
  const bidderRuntime = await createBidderRuntime(hre, intexAuctionContract);

  const series = normalizeSeries(toOptional(args.series));
  const seriesId = resolveSeriesId(series);

  const walletsPath = args.wallets || DEFAULT_WALLETS_PATH;
  const wallets = loadWallets(walletsPath);
  const commitsFile = args.commitsFile || DEFAULT_COMMITS_PATH;

  console.log("\n=== Interactive Auction Flow ===");
  console.log({
    intexAuctionContract,
    escrowAdapterContract,
    paymentTokenContract,
    intexContract,
    seriesId,
    series,
    wallets: wallets.length,
    commitsFile,
  });

  // Step 1: Start auction
  console.log("\n[1/9] Starting auction...");
  const minimumBidPrice = parseFloor(toOptional(args.floor));
  await runAuctionStartCore({
    runtime: auctionRuntime,
    series,
    floor: minimumBidPrice,
    commitEnd: toOptional(args.commitEnd),
    revealEnd: toOptional(args.revealEnd),
    issuanceEnd: toOptional(args.issuanceEnd),
    intexSize: toOptional(args.intexSize),
    intexStrikePrice: toOptional(args.intexStrikePrice),
    coenPriceFloor: toOptional(args.coenPriceFloor),
    minIntexBidQuantity: toOptional(args.minIntexBidQuantity),
  });
  console.log("Auction started");

  // Step 2: Commit bids via script
  console.log("\n[2/9] Committing bids via script...");
  await runCommitBids(bidderRuntime, {
    seriesId,
    wallets,
    commitsFile,
  });
  console.log("Script commits completed");

  // Step 3: PAUSE for manual commit
  console.log("\n[3/9] PAUSE: Please commit your bid manually.");
  console.log(`   Series ID: ${seriesId}`);
  await promptForContinue("Press Enter after you've committed your bid...");

  // Step 4: Start reveal stage
  console.log("\n[4/9] Starting reveal stage...");
  const isGreenDay = parseBoolean(args.isGreenDay, true);
  await runAuctionRevealCore({
    runtime: auctionRuntime,
    series,
    isGreenDay,
  });
  console.log("Reveal stage started");

  // Step 5: Reveal bids via script
  console.log("\n[5/9] Revealing bids via script...");
  await runRevealBids(bidderRuntime, {
    seriesId,
    wallets,
    commitsFile,
    escrowAdapterAddress: escrowAdapterContract,
    paymentTokenAddress: paymentTokenContract,
  });
  console.log("Script reveals completed");

  // Step 6: PAUSE for manual reveal
  console.log("\n[6/9] PAUSE: Please reveal your bid manually.");
  console.log(`   Series ID: ${seriesId}`);
  await promptForContinue("Press Enter after you've revealed your bid...");

  // Step 7: Start clearing stage
  console.log("\n[7/9] Starting clearing stage...");
  await runAuctionClearingCore({
    runtime: auctionRuntime,
    series,
  });
  console.log("Clearing stage started");

  // Step 8: Execute clearing
  console.log("\n[8/9] Executing clearing...");
  await runAuctionExecuteCore({
    runtime: auctionRuntime,
    series,
    issuedIntexCount: toOptional(args.issuedIntexCount),
    clearingPrice: toOptional(args.clearingPrice),
    wonBidsCount: toOptional(args.wonBidsCount),
    finalizeEscrow: parseBoolean(args.finalizeEscrow, true),
    escrowAdapterAddress: escrowAdapterContract,
    paymentTokenAddress: paymentTokenContract,
  });
  console.log("Clearing executed");

  // Step 9: IntexNFT1155 issuance
  console.log("\n[9/9] IntexNFT1155 issuance...");
  try {
    const issuanceRuntime = await createIntex1155IssuanceRuntime(hre, {
      auctionAddress: intexAuctionContract,
      intexAddress: intexContract,
    });
    await runIntex1155IssuanceCore({
      runtime: issuanceRuntime,
      series,
      allocationsPath: undefined,
      skipCreate: false,
      skipMint: false,
    });
    console.log("IntexNFT1155 issuance completed");
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.warn(`[warn] IntexNFT1155 issuance failed: ${message}`);
  }

  console.log("\n=== Interactive Auction Flow Completed ===");
  console.log(`Series ID: ${seriesId}`);
};

// =============================================================================
// Task Definition
// =============================================================================

const auctionInteractive = task("auction-interactive", "Interactive auction flow with manual pauses")
  .addOption({ name: "intexAuctionContract", description: "Auction contract address (required)", defaultValue: "" })
  .addOption({ name: "escrowAdapterContract", description: "EscrowAdapter contract address (required)", defaultValue: "" })
  .addOption({ name: "paymentTokenContract", description: "Stable token contract address (required)", defaultValue: "" })
  .addOption({ name: "intexContract", description: "IntexNFT1155 contract address (required)", defaultValue: "" })
  .addOption({ name: "series", description: "Series in yyyymmdd format", defaultValue: "" })
  .addOption({ name: "floor", description: "Bid floor price", defaultValue: "80000000" })
  .addOption({ name: "commitEnd", description: "Commit stage end timestamp (UNIX)", defaultValue: "" })
  .addOption({ name: "revealEnd", description: "Reveal stage end timestamp (UNIX)", defaultValue: "" })
  .addOption({ name: "issuanceEnd", description: "Issuance stage end timestamp (UNIX)", defaultValue: "" })
  .addOption({ name: "intexSize", description: "Intex size", defaultValue: "1000000" })
  .addOption({ name: "intexStrikePrice", description: "Intex strike price", defaultValue: "1000000000" })
  .addOption({ name: "coenPriceFloor", description: "COEN price floor", defaultValue: "" })
  .addOption({ name: "minIntexBidQuantity", description: "Minimum bid quantity", defaultValue: "12" })
  .addOption({ name: "isGreenDay", description: "Is green day (true/false)", defaultValue: "true" })
  .addOption({ name: "wallets", description: "Path to wallets JSON", defaultValue: DEFAULT_WALLETS_PATH })
  .addOption({ name: "commitsFile", description: "Path to commits JSON", defaultValue: DEFAULT_COMMITS_PATH })
  .addOption({ name: "issuedIntexCount", description: "Override issued Intex count", defaultValue: "" })
  .addOption({ name: "clearingPrice", description: "Override clearing price", defaultValue: "" })
  .addOption({ name: "wonBidsCount", description: "Override won bids count", defaultValue: "" })
  .addOption({ name: "finalizeEscrow", description: "Finalize escrow (true/false)", defaultValue: "true" })
  .setAction(lazy(auctionInteractiveAction));

// =============================================================================
// Export
// =============================================================================

export const auctionFlowTasks = [auctionInteractive.build()];
