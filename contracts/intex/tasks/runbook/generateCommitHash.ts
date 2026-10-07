// Generate Commit Hash Task
// Generates the EIP-712 reveal signature and the matching commit hash
// (`keccak256(signature)`) for manual `IntexAuction` bid operations.

import { task } from "hardhat/config";
import { keccak256, isAddress, type Hex } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { resolveWorldwideDay } from "../../scripts/shared/auctionId.js";
import { lazy, toOptional } from "../../scripts/shared/taskUtils.js";

// =============================================================================
// Types
// =============================================================================

interface GenerateCommitHashTaskArgs {
  worldwideDay?: string;
  units?: string;
  bidRate?: string;
  issuanceCurrency?: string;
  referenceCurrency?: string;
  chainId?: string;
  auctionContract?: string;
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

  // Parse parameters. `--worldwide-day` (yyyymmdd) resolves to the uint32 worldwide day
  // that keys the auction; it falls back to today's date when omitted.
  const worldwideDay = resolveWorldwideDay(toOptional(args.worldwideDay));
  const bidder = account.address;
  const units = BigInt(toOptional(args.units) || "5");
  const bidRate = BigInt(toOptional(args.bidRate) || "800000");
  const issuanceCurrency = Number(toOptional(args.issuanceCurrency) || "840");
  const referenceCurrency = Number(toOptional(args.referenceCurrency) || "840");
  const chainId = BigInt(toOptional(args.chainId) || "97");

  // The EIP-712 domain binds the deployment address. The signature is invalid against any
  // other `IntexAuction` instance.
  const verifyingContract = (toOptional(args.auctionContract) ||
    process.env.INTEX_AUCTION_ADDRESS) as `0x${string}` | undefined;
  if (!verifyingContract || !isAddress(verifyingContract)) {
    console.error(
      "Error: pass --auction-contract 0x... (IntexAuction address) or set INTEX_AUCTION_ADDRESS",
    );
    process.exit(1);
  }

  // Log inputs
  console.log("\n=== Generating Commit Hash (EIP-712) ===");
  console.log("worldwideDay:", worldwideDay);
  console.log("bidder:", bidder);
  console.log("units:", units.toString());
  console.log("bidRate:", bidRate.toString());
  console.log("issuanceCurrency:", issuanceCurrency);
  console.log("referenceCurrency:", referenceCurrency);
  console.log("chainId:", chainId.toString());
  console.log("verifyingContract:", verifyingContract);

  // EIP-712 typed data. It must mirror `IntexAuction.REVEAL_BID_TYPEHASH` and the
  // contract's EIP712("IntexAuction", "1") domain.
  const signature = await account.signTypedData({
    domain: {
      name: "IntexAuction",
      version: "1",
      chainId: Number(chainId),
      verifyingContract,
    },
    types: {
      RevealBid: [
        { name: "worldwideDay", type: "uint32" },
        { name: "bidder", type: "address" },
        { name: "units", type: "uint16" },
        { name: "bidRate", type: "uint32" },
        { name: "issuanceCurrency", type: "uint16" },
        { name: "referenceCurrency", type: "uint16" },
      ],
    },
    primaryType: "RevealBid",
    message: {
      worldwideDay,
      bidder,
      units: Number(units),
      bidRate: Number(bidRate),
      issuanceCurrency,
      referenceCurrency,
    },
  });
  console.log("\nsignature:", signature);

  const commitHash = keccak256(signature);
  console.log("\n=== RESULT ===");
  console.log("commitHash:", commitHash);

  console.log("\n=== FOR REVEAL ===");
  console.log(`worldwideDay: ${worldwideDay}`);
  console.log(`units: ${units}`);
  console.log(`bidRate: ${bidRate}`);
  console.log(`issuanceCurrency: ${issuanceCurrency}`);
  console.log(`referenceCurrency: ${referenceCurrency}`);
  console.log(`chainId: ${chainId}`);
  console.log(`signature: ${signature}`);
};

// =============================================================================
// Task Definition
// =============================================================================

const generateCommitHash = task(
  "generate-commit-hash",
  "Generate the EIP-712 reveal signature and commit hash for manual bidding",
)
  .addOption({
    name: "worldwideDay",
    description: "Worldwide day in yyyymmdd format (e.g. 20260501). Resolves to the uint32 auction key; defaults to today.",
    defaultValue: "",
  })
  .addOption({ name: "units", description: "Intex units", defaultValue: "5" })
  .addOption({ name: "bidRate", description: "Bid rate (1e6 fixed-point, % of strike)", defaultValue: "800000" })
  .addOption({ name: "issuanceCurrency", description: "Issuance ISO currency code", defaultValue: "840" })
  .addOption({ name: "referenceCurrency", description: "Priced reference ISO currency code", defaultValue: "840" })
  .addOption({ name: "chainId", description: "Chain ID", defaultValue: "97" })
  .addOption({
    name: "auctionContract",
    description: "IntexAuction address (EIP-712 verifyingContract; default: INTEX_AUCTION_ADDRESS env)",
    defaultValue: "",
  })
  .setAction(lazy(generateCommitHashAction));

// =============================================================================
// Export
// =============================================================================

export const generateCommitHashTasks = [generateCommitHash.build()];
