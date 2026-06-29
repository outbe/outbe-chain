// Generate Commit Hash Script
// Standalone script to generate the EIP-712 reveal signature and the matching commit hash
// (`keccak256(signature)`) for manual `IntexAuction` bid operations.

import { keccak256, isAddress, type Hex } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { parseArgs } from "../shared/parseArgs.js";

// =============================================================================
// Main
// =============================================================================

async function main() {
  const params = parseArgs();

  // Get private key from environment
  const privateKey = (process.env.BSC_TESTNET_PRIVATE_KEY || process.env.PRIVATE_KEY) as Hex;
  if (!privateKey) {
    console.error("Error: Set BSC_TESTNET_PRIVATE_KEY or PRIVATE_KEY environment variable");
    process.exit(1);
  }

  const account = privateKeyToAccount(privateKey);

  // Parse parameters
  const seriesIdStr = params.seriesId || params.series || "20260108";
  if (!/^\d{8}$/.test(seriesIdStr)) {
    console.error("Error: --series must be yyyymmdd, e.g. 20260108");
    process.exit(1);
  }
  const seriesId = parseInt(seriesIdStr, 10);
  const bidder = (params.bidder || account.address) as `0x${string}`;
  const quantity = BigInt(params.quantity || "5");
  const bidRate = BigInt(params.bidRate || params.rate || "800000");
  const chainId = BigInt(params.chainId || "97");

  // EIP-712 domain binds the deployment address; signature is invalid against any other
  // `IntexAuction` instance.
  const verifyingContract = (params.contract || params.verifyingContract ||
    process.env.INTEX_AUCTION_ADDRESS) as `0x${string}` | undefined;
  if (!verifyingContract || !isAddress(verifyingContract)) {
    console.error(
      "Error: pass --contract=0x... (IntexAuction address) or set INTEX_AUCTION_ADDRESS",
    );
    process.exit(1);
  }

  // Log inputs
  console.log("\n=== Generating Commit Hash (EIP-712) ===");
  console.log("seriesId:", seriesId);
  console.log("bidder:", bidder);
  console.log("quantity:", quantity.toString());
  console.log("bidRate:", bidRate.toString());
  console.log("chainId:", chainId.toString());
  console.log("verifyingContract:", verifyingContract);

  // EIP-712 typed data — must mirror `IntexAuction.REVEAL_BID_TYPEHASH` and the contract's
  // EIP712("IntexAuction", "1") domain.
  const signature = await account.signTypedData({
    domain: {
      name: "IntexAuction",
      version: "1",
      chainId: Number(chainId),
      verifyingContract,
    },
    types: {
      RevealBid: [
        { name: "seriesId", type: "uint32" },
        { name: "bidder", type: "address" },
        { name: "quantity", type: "uint16" },
        { name: "bidRate", type: "uint32" },
      ],
    },
    primaryType: "RevealBid",
    message: {
      seriesId,
      bidder,
      quantity: Number(quantity),
      bidRate: Number(bidRate),
    },
  });
  console.log("\nsignature:", signature);

  const commitHash = keccak256(signature);
  console.log("\n=== RESULT ===");
  console.log("commitHash:", commitHash);

  console.log("\n=== FOR REVEAL ===");
  console.log(`seriesId: ${seriesId}`);
  console.log(`quantity: ${quantity}`);
  console.log(`bidRate: ${bidRate}`);
  console.log(`chainId: ${chainId}`);
  console.log(`signature: ${signature}`);
}

main().catch(console.error);
