// Save deployed contract addresses from Hardhat Ignition deployments
// Reads from ignition/deployments and outputs to a structured JSON file

import * as fs from "fs";
import * as path from "path";

/** Parse CLI arguments: --key value or --key=value */
function parseArgs(): Record<string, string> {
  const args = process.argv.slice(2);
  const params: Record<string, string> = {};
  for (let i = 0; i < args.length; i++) {
    if (!args[i].startsWith("--")) continue;
    const arg = args[i].slice(2);
    if (arg.includes("=")) {
      const [key, value] = arg.split("=");
      params[key] = value;
      continue;
    }
    const value = args[i + 1] && !args[i + 1].startsWith("--") ? args[i + 1] : "";
    params[arg] = value;
    if (value) i++;
  }
  return params;
}

const IGNITION_DEPLOYMENTS_DIR = "ignition/deployments";

interface DeployedAddresses {
  network: string;
  chainId: number;
  deployedAt: string;
  commitHash: string;
  contracts: Record<string, string>;
}

interface IgnitionDeployedAddresses {
  [key: string]: string;
}

// Map network names to chain IDs
const CHAIN_IDS: Record<string, number> = {
  bscTestnet: 97,
  bsc: 56,
  outbeTestnet: 512215,
  outbeTestnetNew: 54322345,
  outbePrivnet: 512512,
  outbeDevnet: 424242,
};

// Module name to contract name mapping
const MODULE_TO_CONTRACT: Record<string, string> = {
  "IntexAuctionModule#IntexAuction": "IntexAuction",
  "EscrowAdapterModule#EscrowAdapter": "EscrowAdapter",
  "IntexNFT1155Module#IntexNFT1155": "IntexNFT1155",
  "ONFT1155AdapterModule#ONFT1155Adapter": "ONFT1155Adapter",
  "ONFT1155AdapterBatchModule#ONFT1155AdapterBatch": "ONFT1155AdapterBatch",
  "TargetMessengerModule#TargetMessenger": "TargetMessenger",
  "OriginMessengerModule#OriginMessenger": "OriginMessenger",
  "DesisModule#Desis": "Desis",
  "IntexFactoryModule#IntexFactory": "IntexFactory",
  "MockPromisModule#MockPromis": "MockPromis",
  "MockPromisLimitModule#MockPromisLimit": "MockPromisLimit",
  "IntexSettlementModule#IntexSettlement": "IntexSettlement",
};

function collectAddresses(network: string): Record<string, string> {
  const deploymentsDir = IGNITION_DEPLOYMENTS_DIR;
  const addresses: Record<string, string> = {};

  if (!fs.existsSync(deploymentsDir)) {
    console.warn("No deployments directory found");
    return addresses;
  }

  const deploymentDirs = fs.readdirSync(deploymentsDir);

  for (const dir of deploymentDirs) {
    // Try both structures: direct and with chain-{chainId} subdirectory
    const possiblePaths = [
      path.join(deploymentsDir, dir, "deployed_addresses.json"),
      path.join(deploymentsDir, dir, `chain-${CHAIN_IDS[network]}`, "deployed_addresses.json"),
    ];

    for (const deployedAddressesPath of possiblePaths) {
      if (fs.existsSync(deployedAddressesPath)) {
        const deployed: IgnitionDeployedAddresses = JSON.parse(
          fs.readFileSync(deployedAddressesPath, "utf-8")
        );

        for (const [moduleKey, address] of Object.entries(deployed)) {
          const contractName = MODULE_TO_CONTRACT[moduleKey] || moduleKey.split("#")[1] || moduleKey;
          addresses[contractName] = address;
        }
        break; // Found addresses in this deployment dir
      }
    }
  }

  return addresses;
}

function main(): void {
  const args = parseArgs();
  const network = args["network"];
  const outputPath = args["output"] || "deployed-addresses.json";

  if (!network) {
    console.error("Error: --network is required");
    console.error("Usage: ts-node save-addresses.ts --network <network> [--output <path>]");
    process.exit(1);
  }

  if (!CHAIN_IDS[network]) {
    console.error(`Error: Unknown network '${network}'`);
    console.error(`Valid networks: ${Object.keys(CHAIN_IDS).join(", ")}`);
    process.exit(1);
  }

  console.log(`Collecting deployed addresses for ${network}...`);

  const contracts = collectAddresses(network);

  if (Object.keys(contracts).length === 0) {
    console.warn("Warning: No deployed contracts found");
  }

  const output: DeployedAddresses = {
    network,
    chainId: CHAIN_IDS[network],
    deployedAt: new Date().toISOString(),
    commitHash: process.env.GITHUB_SHA || "local",
    contracts,
  };

  fs.writeFileSync(outputPath, JSON.stringify(output, null, 2));
  console.log(`\n✓ Saved addresses to ${outputPath}`);
  console.log(`\nContracts found: ${Object.keys(contracts).length}`);
  
  for (const [name, address] of Object.entries(contracts)) {
    console.log(`  ${name}: ${address}`);
  }
}

main();
