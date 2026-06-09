// Auction Bidders Tasks
// Hardhat tasks for committing and revealing bids.

import { task } from "hardhat/config";
import { runCommitBids, runRevealBids } from "../../scripts/auction/bidders.js";
import { createBidderRuntime } from "../../scripts/shared/runtime.js";
import { resolveSeriesId, parseRange } from "../../scripts/shared/auctionId.js";
import { loadWallets, DEFAULT_WALLETS_PATH, DEFAULT_COMMITS_PATH } from "../../scripts/shared/wallets.js";
import { lazy, toOptional } from "../../scripts/shared/taskUtils.js";
import type { BiddersCommitTaskArgs, BiddersRevealTaskArgs } from "../types.js";

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
// Task Actions
// =============================================================================

const commitBidsAction = async (args: BiddersCommitTaskArgs, hre: unknown) => {
  const intexAuctionContract = requireAddress(toOptional(args.intexAuctionContract), "auction-contract");
  const runtime = await createBidderRuntime(hre, intexAuctionContract);
  const walletsPath = args.wallets || DEFAULT_WALLETS_PATH;
  const wallets = loadWallets(walletsPath);
  const seriesId = resolveSeriesId(toOptional(args.series));
  const quantityRange = parseRange(args.qtyRange);
  const commitsFile = args.commitsFile || DEFAULT_COMMITS_PATH;

  // Parse priceRange as strings (e.g., "1.08-1.18")
  const priceRange: [string, string] | undefined = args.priceRange
    ? (args.priceRange.split("-").map((s) => s.trim()) as [string, string])
    : undefined;

  await runCommitBids(runtime, {
    seriesId,
    wallets,
    quantityRange,
    priceRange,
    commitsFile,
  });
};

const revealBidsAction = async (args: BiddersRevealTaskArgs, hre: unknown) => {
  const intexAuctionContract = requireAddress(toOptional(args.intexAuctionContract), "auction-contract");
  const escrowAdapterContract = requireAddress(toOptional(args.escrowAdapterContract), "escrow-adapter-contract");
  const paymentTokenContract = requireAddress(toOptional(args.paymentTokenContract), "payment-token-contract");

  const runtime = await createBidderRuntime(hre, intexAuctionContract);
  const walletsPath = args.wallets || DEFAULT_WALLETS_PATH;
  const wallets = loadWallets(walletsPath);
  const seriesId = resolveSeriesId(toOptional(args.series));
  const commitsFile = args.commitsFile || DEFAULT_COMMITS_PATH;

  await runRevealBids(runtime, {
    seriesId,
    wallets,
    commitsFile,
    escrowAdapterAddress: escrowAdapterContract,
    paymentTokenAddress: paymentTokenContract,
  });
};

// =============================================================================
// Task Definitions
// =============================================================================

const commitBids = task("bidders-commit", "Commit bids for multiple bidders")
  .addOption({ name: "intexAuctionContract", description: "Auction contract address (required)", defaultValue: "" })
  .addOption({ name: "series", description: "Series in yyyymmdd format", defaultValue: "today" })
  .addOption({ name: "wallets", description: "Path to wallets JSON", defaultValue: DEFAULT_WALLETS_PATH })
  .addOption({ name: "qtyRange", description: "Quantity range (min-max), defaults to [bidMinimumQuantity, bidMinimumQuantity+5]", defaultValue: "" })
  .addOption({ name: "priceRange", description: "Price range in USDC (min-max)", defaultValue: "" })
  .addOption({ name: "commitsFile", description: "Path to commits JSON", defaultValue: DEFAULT_COMMITS_PATH })
  .setAction(lazy(commitBidsAction));

const revealBids = task("bidders-reveal", "Reveal bids for multiple bidders")
  .addOption({ name: "intexAuctionContract", description: "Auction contract address (required)", defaultValue: "" })
  .addOption({ name: "escrowAdapterContract", description: "EscrowAdapter contract address (required)", defaultValue: "" })
  .addOption({ name: "paymentTokenContract", description: "Stable token contract address (required)", defaultValue: "" })
  .addOption({ name: "series", description: "Series in yyyymmdd format", defaultValue: "today" })
  .addOption({ name: "wallets", description: "Path to wallets JSON", defaultValue: DEFAULT_WALLETS_PATH })
  .addOption({ name: "commitsFile", description: "Path to commits JSON", defaultValue: DEFAULT_COMMITS_PATH })
  .setAction(lazy(revealBidsAction));

// =============================================================================
// Export
// =============================================================================

export const auctionBidderTasks = [commitBids.build(), revealBids.build()];
