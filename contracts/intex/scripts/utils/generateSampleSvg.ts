import { network } from "hardhat";
import * as fs from "fs";

async function main() {
  const { viem } = await network.connect();
  const [deployer, bridger] = await viem.getWalletClients();

  // Deploy contract
  const nft = await viem.deployContract("IntexNFT1155", [
    deployer.account.address,
    bridger.account.address,
  ]);

  // The IntexNFT1155 write surface, mirroring the locked on-chain ABI.
  const nftWrite = nft.write as unknown as {
    createSeries: (
      args: [
        number,
        number,
        bigint,
        bigint,
        bigint,
        number,
        number,
        { windowDays: number; thresholdDays: number; callPriceMinor: bigint },
      ],
      opts: { account: string },
    ) => Promise<`0x${string}`>;
    mint: (
      args: [string, bigint, number],
      opts: { account: string },
    ) => Promise<`0x${string}`>;
  };

  // Create a series with realistic data (seriesId in yyyymmdd format)
  const SERIES_ID = 20260401;
  const issuedIntexCount = 10_000; // Sample cap; cap is enforced on mint.

  const promisLoadMinor = 1_000_000n; // 1M promis per intex
  const costAmountMinor = 1000n * 10n ** 6n; // $1,000 (6 decimals)
  const floorPriceMinor = 70n * 10n ** 6n; // $70 (6 decimals)
  const settlementTokenAlias = 840; // ISO 4217 numeric alias (840 = USD)
  const callTrigger = { windowDays: 0, thresholdDays: 0, callPriceMinor: 0n };

  await nftWrite.createSeries(
    [SERIES_ID, issuedIntexCount, promisLoadMinor, costAmountMinor, floorPriceMinor, 0, settlementTokenAlias, callTrigger],
    { account: bridger.account.address },
  );

  // Mint some tokens
  await nftWrite.mint(
    [deployer.account.address, 100n, SERIES_ID],
    { account: bridger.account.address }
  );

  // Get the URI — Issued token id for a series is uint256(seriesId)
  const tokenId = BigInt(SERIES_ID);
  const uri = await nft.read.uri([tokenId]);

  // Parse the data URI
  const base64Json = uri.replace("data:application/json;base64,", "");
  const json = JSON.parse(Buffer.from(base64Json, "base64").toString("utf-8"));
  
  // Extract SVG from image field
  const base64Svg = json.image.replace("data:image/svg+xml;base64,", "");
  const svg = Buffer.from(base64Svg, "base64").toString("utf-8");

  // Save SVG to file
  fs.writeFileSync("sample-nft.svg", svg);
  console.log("SVG saved to sample-nft.svg");
  
  // Also save the full metadata
  fs.writeFileSync("sample-nft-metadata.json", JSON.stringify(json, null, 2));
  console.log("Metadata saved to sample-nft-metadata.json");
  
  console.log("\nMetadata attributes:");
  console.log(JSON.stringify(json.attributes, null, 2));
}

main().catch(console.error);
