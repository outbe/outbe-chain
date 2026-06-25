// Generate Commit Hash Task
// Generates commit hash and signature for manual bid operations.

import { task } from "hardhat/config";
import { keccak256, encodePacked, toHex, concat, type Hex } from "viem";
import { sign, privateKeyToAccount } from "viem/accounts";
import { resolveSeriesId } from "../../scripts/shared/auctionId.js";
import { lazy, toOptional } from "../../scripts/shared/taskUtils.js";

// =============================================================================
// Types
// =============================================================================

interface GenerateCommitHashTaskArgs {
  series?: string;
  bidder?: string;
  quantity?: string;
  bidRate?: string;
  chainId?: string;
}

// =============================================================================
// Signature Helpers
// =============================================================================

function createMessageHash(
  seriesId: number,
  bidder: `0x${string}`,
  quantity: bigint,
  bidRate: bigint,
  chainId: bigint,
): Hex {
  return keccak256(
    encodePacked(
      ["uint32", "address", "uint16", "uint32", "uint64"],
      [seriesId, bidder, Number(quantity), Number(bidRate), chainId],
    ),
  );
}

function createEthSignedMessageHash(messageHash: Hex): Hex {
  return keccak256(
    encodePacked(["string", "bytes32"], ["\x19Ethereum Signed Message:\n32", messageHash]),
  );
}

function serializeSignature(sig: { r: Hex; s: Hex; v?: bigint; yParity?: number }): Hex {
  const v = sig.v ?? (sig.yParity === 1 ? 28n : 27n);
  return concat([sig.r, sig.s, toHex(v, { size: 1 })]);
}

// =============================================================================
// Task Action
// =============================================================================

const generateCommitHashAction = async (args: GenerateCommitHashTaskArgs) => {
  // Get private key from environment
  const privateKey = (process.env.BSC_TESTNET_PRIVATE_KEY || process.env.PRIVATE_KEY) as Hex;
  if (!privateKey) {
    console.error("Error: Set BSC_TESTNET_PRIVATE_KEY or PRIVATE_KEY environment variable");
    process.exit(1);
  }

  const account = privateKeyToAccount(privateKey);

  // Parse parameters. `--series` (yyyymmdd) resolves to the uint32 seriesId that
  // keys the auction; it falls back to today's date when omitted.
  const seriesId = resolveSeriesId(toOptional(args.series));
  const bidder = (toOptional(args.bidder) || account.address) as `0x${string}`;
  const quantity = BigInt(toOptional(args.quantity) || "5");
  const bidRate = BigInt(toOptional(args.bidRate) || "800000");
  const chainId = BigInt(toOptional(args.chainId) || "97");

  // Log inputs
  console.log("\n=== Generating Commit Hash ===");
  console.log("seriesId:", seriesId);
  console.log("bidder:", bidder);
  console.log("quantity:", quantity.toString());
  console.log("bidRate:", bidRate.toString());
  console.log("chainId:", chainId.toString());

  // Generate hashes and signature
  const messageHash = createMessageHash(seriesId, bidder, quantity, bidRate, chainId);
  console.log("\nmessageHash:", messageHash);

  const ethSignedMessageHash = createEthSignedMessageHash(messageHash);
  console.log("ethSignedMessageHash:", ethSignedMessageHash);

  const sig = await sign({ hash: ethSignedMessageHash, privateKey });
  const signature = serializeSignature(sig);
  console.log("signature:", signature);

  const commitHash = keccak256(signature);
  console.log("\n=== RESULT ===");
  console.log("commitHash:", commitHash);

  console.log("\n=== FOR REVEAL ===");
  console.log(`seriesId: ${seriesId}`);
  console.log(`quantity: ${quantity}`);
  console.log(`bidRate: ${bidRate}`);
  console.log(`chainId: ${chainId}`);
  console.log(`signature: ${signature}`);
};

// =============================================================================
// Task Definition
// =============================================================================

const generateCommitHash = task("generate-commit-hash", "Generate commit hash and signature for manual bidding")
  .addOption({
    name: "series",
    description: "Series in yyyymmdd format (e.g. 20260501). Resolves to the uint32 seriesId; defaults to today.",
    defaultValue: "",
  })
  .addOption({ name: "bidder", description: "Bidder address (default: from PRIVATE_KEY)", defaultValue: "" })
  .addOption({ name: "quantity", description: "Intex quantity", defaultValue: "5" })
  .addOption({ name: "bidRate", description: "Bid rate (1e6 fixed-point, % of strike)", defaultValue: "800000" })
  .addOption({ name: "chainId", description: "Chain ID", defaultValue: "97" })
  .setAction(lazy(generateCommitHashAction));

// =============================================================================
// Export
// =============================================================================

export const generateCommitHashTasks = [generateCommitHash.build()];
