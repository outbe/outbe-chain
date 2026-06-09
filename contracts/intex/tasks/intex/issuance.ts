// IntexNFT1155 Issuance Task
// Creates IntexNFT1155 series from completed auction params and mints tokens to recipients.
// Recipients can be from revealed bids or from a custom allocations JSON file.

import { task } from "hardhat/config";
import { runIntex1155IssuanceCore } from "../../scripts/intex/issuance.js";
import { createIntex1155IssuanceRuntime } from "../../scripts/shared/runtime.js";
import { lazy, toOptional } from "../../scripts/shared/taskUtils.js";
import type { Intex1155IssuanceTaskArgs } from "../types.js";

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
// Task Action
// =============================================================================

const intex1155IssuanceAction = async (args: Intex1155IssuanceTaskArgs, hre: unknown) => {
  const intexAuctionContract = requireAddress(toOptional(args.intexAuctionContract), "auction-contract");
  const intexContract = requireAddress(toOptional(args.intexContract), "intex-contract");

  const runtime = await createIntex1155IssuanceRuntime(hre, {
    auctionAddress: intexAuctionContract,
    intexAddress: intexContract,
  });

  await runIntex1155IssuanceCore({
    runtime,
    series: toOptional(args.series),
    allocationsPath: toOptional(args.allocations),
    skipCreate: Boolean(args.skipCreate),
    skipMint: Boolean(args.skipMint),
  });
};

// =============================================================================
// Task Definition
// =============================================================================

const intex1155Issuance = task(
  "intex1155-issuance",
  "Create IntexNFT1155 series from auction params and mint to recipients",
)
  .addOption({ name: "intexAuctionContract", description: "Auction contract address (required)", defaultValue: "" })
  .addOption({ name: "intexContract", description: "IntexNFT1155 contract address (required)", defaultValue: "" })
  .addOption({ name: "series", description: "Series in yyyymmdd format", defaultValue: "" })
  .addOption({ name: "allocations", description: "Path to allocations JSON file", defaultValue: "" })
  .addFlag({ name: "skipCreate", description: "Skip createSeries (series already exists)" })
  .addFlag({ name: "skipMint", description: "Skip mintBatch (only create series)" })
  .setAction(lazy(intex1155IssuanceAction));

// =============================================================================
// Export
// =============================================================================

export const intex1155IssuanceTasks = [intex1155Issuance.build()];
