// Check Auction State Script
// Standalone script to inspect auction state, schedule, commits, and revealed bids.
// Usage: npx hardhat run scripts/auction/checkState.ts -- --series "20260108" --bidder "0x..."

import { createPublicClient, http } from "viem";
import { bscTestnet } from "viem/chains";
import * as fs from "fs";
import { parseArgs } from "../shared/parseArgs.js";
import { seriesIdToUint32, normalizeSeries } from "../shared/auctionId.js";

async function main() {
  const params = parseArgs();

  const seriesStr = normalizeSeries(params.series || params.seriesId);
  const seriesId = seriesIdToUint32(seriesStr);
  const contractAddress = (params.contract || params.address || "0x649053a29f9d20574D1eB8d54B7F3bB77f07F734") as `0x${string}`;
  const bidder = (params.bidder || "0x78e5Bbd38feC73C39F0E798823EDEAD22552fdCf") as `0x${string}`;

  const pc = createPublicClient({
    chain: bscTestnet,
    transport: http(),
  });
  const artifact = JSON.parse(fs.readFileSync("./artifacts/contracts/bnb/IntexAuction.sol/IntexAuction.json", "utf8"));
  const abi = artifact.abi;

  const contract = { address: contractAddress, abi };

  console.log("\n=== Checking Auction ===");
  console.log("seriesId:", seriesId);
  console.log("contract:", contractAddress);
  console.log("bidder:", bidder);

  try {
    let info: any;
    let exists = true;
    try {
      info = await pc.readContract({
        ...contract,
        functionName: "getAuctionInfo",
        args: [seriesId],
      });
    } catch {
      exists = false;
    }

    console.log("\nAuction exists:", exists);

    if (exists) {
      const stage = await pc.readContract({
        ...contract,
        functionName: "getAuctionStage",
        args: [seriesId],
      });
      const stageNames = ["CommittingBids", "RevealingBids", "Issuance", "Completed", "Cancelled"];
      const worldwideDayNames = ["Unknown", "Green", "Red"];
      console.log("Stage:", stageNames[Number(stage)] || stage);
      console.log("WorldwideDayState:", worldwideDayNames[Number(info.worldwideDayState)] ?? info.worldwideDayState);
      console.log("minIntexBidPrice:", info.params?.minIntexBidPrice?.toString());
      console.log("minIntexBidQuantity:", info.params?.minIntexBidQuantity?.toString());

      const counts = await pc.readContract({
        ...contract,
        functionName: "auctionRunningCounts",
        args: [seriesId],
      }) as any;
      console.log("committedBidsCount:", counts.committedBidsCount?.toString() ?? counts[0]?.toString());
      console.log("revealedBidsCount:", counts.revealedBidsCount?.toString() ?? counts[1]?.toString());

      // Schedule
      const now = Math.floor(Date.now() / 1000);
      console.log("\n=== Schedule ===");
      console.log("Now:", now, new Date(now * 1000).toISOString());
      console.log("commitEnd:", info.schedule?.commitEnd?.toString(), info.schedule?.commitEnd ? new Date(Number(info.schedule.commitEnd) * 1000).toISOString() : "");
      console.log("revealEnd:", info.schedule?.revealEnd?.toString(), info.schedule?.revealEnd ? new Date(Number(info.schedule.revealEnd) * 1000).toISOString() : "");
      console.log("issuanceEnd:", info.schedule?.issuanceEnd?.toString(), info.schedule?.issuanceEnd ? new Date(Number(info.schedule.issuanceEnd) * 1000).toISOString() : "");

      // Check if bidder already committed
      const zeroBytes32 = "0x0000000000000000000000000000000000000000000000000000000000000000";
      const commitHash = await pc.readContract({
        ...contract,
        functionName: "committedBidsByHash",
        args: [seriesId, bidder],
      });
      console.log("\nBidder commit hash:", commitHash);
      console.log("Already committed:", commitHash !== zeroBytes32);

      // Get auction details with bids
      const [, bids] = await pc.readContract({
        ...contract,
        functionName: "getAuctionDetails",
        args: [seriesId],
      }) as any;
      console.log("\nRevealed bids count:", bids.length);
      if (bids.length > 0) {
        console.log("\n=== Revealed Bids ===");
        for (const bid of bids) {
          console.log({
            bidder: bid.bidderAddress,
            intexQuantity: bid.intexQuantity?.toString(),
            intexBidPrice: bid.intexBidPrice?.toString(),
          });
        }
      }
    }
  } catch (e: any) {
    console.log("Error:", e.message);
  }
}

main().catch(console.error);
