// Dynamically resolve Ignition deployment parameters
/* 
 * Sources:
 * 1. Environment variables (DEPLOYER_ADDRESS, BRIDGER_ADDRESS)
 * 2. Previously deployed contract addresses from @outbe/intex-contracts package
 * 3. LayerZero endpoint addresses per network
 * 4. Defaults (ADDRESS_ZERO for optional params)
 */

import * as fs from "fs";

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

const ADDRESS_ZERO = "0x0000000000000000000000000000000000000000";

// Custom LZ V2 Endpoint — same CREATE2 address on every network (BSC, Outbe, etc.)
const LZ_ENDPOINTS: Record<string, string> = {
  bscTestnet: "0x2915f5C5835576CC6E4bFBc002519E2B37cCABe2",
  bsc: "0x2915f5C5835576CC6E4bFBc002519E2B37cCABe2",
  outbePrivnet: "0x2915f5C5835576CC6E4bFBc002519E2B37cCABe2",
  outbeDevnet: "0x2915f5C5835576CC6E4bFBc002519E2B37cCABe2",
  outbeTestnet: "0x2915f5C5835576CC6E4bFBc002519E2B37cCABe2",
  outbeTestnetNew: "0x2915f5C5835576CC6E4bFBc002519E2B37cCABe2",
};

// LayerZero Endpoint IDs (for cross-chain bridge adapter deployment)
const LZ_EIDS: Record<string, number> = {
  bscTestnet: 40102,
  bsc: 30102,
  outbePrivnet: 40512,
  outbeDevnet: 40712,
  outbeTestnet: 40812,
  outbeTestnetNew: 40912,
};

interface PackageAddresses {
  contracts: Record<string, string>;
}

interface IgnitionParameters {
  [moduleName: string]: Record<string, string | number>;
}

// Contracts in each scope
const SCOPES: Record<string, string[]> = {
  bscCore: ["IntexNFT1155", "EscrowAdapter", "IntexAuction"],
  outbeCore: ["IntexNFT1155", "IntexSettlement", "Desis", "IntexFactory"],
  bscBridge: ["ONFT1155Adapter", "ONFT1155AdapterBatch", "TargetMessenger"],
  outbeBridge: ["ONFT1155Adapter", "ONFT1155AdapterBatch", "OriginMessenger"],
  outbeMocks: ["MockPromis", "MockPromisLimit"],
};

function loadPackageAddresses(network: string): Record<string, string> {
  // Try to load from installed package
  const possiblePaths = [
    `node_modules/@outbe/intex-contracts/dist/addresses/${network}.json`,
    `node_modules/@outbe/intex-contracts/addresses/${network}.json`,
  ];

  for (const p of possiblePaths) {
    if (fs.existsSync(p)) {
      try {
        const data: PackageAddresses = JSON.parse(fs.readFileSync(p, "utf-8"));
        console.error(`Loaded addresses from ${p}`);
        return data.contracts || {};
      } catch {
        console.error(`Failed to parse ${p}`);
      }
    }
  }

  console.error("No package addresses found, using empty addresses");
  return {};
}

function resolveParameters(
  network: string,
  scope: string,
  selectedContracts: string[],
  packageAddresses: Record<string, string>,
  targetChain: string
): IgnitionParameters {
  const deployerAddress = process.env.DEPLOYER_ADDRESS || ADDRESS_ZERO;
  const bridgerAddress = process.env.BRIDGER_ADDRESS || ADDRESS_ZERO;

  const lzEndpoint = LZ_ENDPOINTS[network] || ADDRESS_ZERO;

  // Determine which contracts to include
  let contractsToInclude: string[];
  if (scope === "selective" && selectedContracts.length > 0) {
    contractsToInclude = selectedContracts;
  } else {
    contractsToInclude = SCOPES[scope] || SCOPES.bscCore;
  }

  const params: IgnitionParameters = {};

  // Only include parameters for contracts being deployed
  for (const contract of contractsToInclude) {
    switch (contract.toLowerCase()) {
      case "intexauction":
        params.IntexAuctionModule = {
          deployer: deployerAddress,
          bridger: bridgerAddress,
        };
        break;

      case "escrowadapter":
        params.EscrowAdapterModule = {
          deployer: deployerAddress,
          bridger: bridgerAddress,
        };
        break;

      case "intexnft1155":
        params.IntexNFT1155Module = {
          defaultAdmin: deployerAddress,
          bridger: bridgerAddress,
        };
        break;

      case "onft1155adapter":
        params.ONFT1155AdapterModule = {
          token: packageAddresses.IntexNFT1155 || ADDRESS_ZERO,
          lzEndpoint,
          delegate: deployerAddress,
          outbeEid: network.startsWith("outbe")
            ? 0
            : (targetChain && LZ_EIDS[targetChain] ? LZ_EIDS[targetChain] : LZ_EIDS.outbeTestnet),
        };
        break;

      case "onft1155adapterbatch":
        params.ONFT1155AdapterBatchModule = {
          token: packageAddresses.IntexNFT1155 || ADDRESS_ZERO,
          lzEndpoint,
          delegate: deployerAddress,
        };
        break;

      case "targetmessenger":
        params.TargetMessengerModule = {
          lzEndpoint,
          delegate: deployerAddress,
          outbeEid: targetChain && LZ_EIDS[targetChain] ? LZ_EIDS[targetChain] : LZ_EIDS.outbeTestnet,
        };
        break;

      case "originmessenger":
        params.OriginMessengerModule = {
          lzEndpoint,
          delegate: deployerAddress,
          bnbEid: targetChain && LZ_EIDS[targetChain] ? LZ_EIDS[targetChain] : LZ_EIDS.bscTestnet,
        };
        break;

      case "intexsettlement":
        params.IntexSettlementModule = {
          defaultAdmin: deployerAddress,
        };
        break;

      case "desis":
        params.DesisModule = {
          defaultAdmin: deployerAddress,
          bridger: bridgerAddress,
        };
        break;

      case "intexfactory":
        params.IntexFactoryModule = {
          defaultAdmin: deployerAddress,
        };
        break;

      case "mockpromis":
        params.MockPromisModule = {
          defaultAdmin: deployerAddress,
        };
        break;

      case "mockpromislimit":
        params.MockPromisLimitModule = {
          defaultAdmin: deployerAddress,
        };
        break;
    }
  }

  return params;
}

function main(): void {
  const args = parseArgs();
  const network = args["network"];
  const scope = args["scope"] || "core";
  const selected = args["selected"] || "";
  const targetChain = args["target"] || "";

  if (!network) {
    console.error("Error: --network is required");
    console.error("Usage: ts-node resolve-parameters.ts --network <network> [--scope <scope>] [--selected <contracts>] [--target <chain>]");
    process.exit(1);
  }

  const selectedContracts = selected
    .split(/[\s,]+/)
    .map((s) => s.trim())
    .filter(Boolean);

  // Load existing addresses from package (if available)
  const packageAddresses = loadPackageAddresses(network);

  // Resolve parameters
  const params = resolveParameters(network, scope, selectedContracts, packageAddresses, targetChain);

  // Output JSON to stdout (for piping to file in workflow)
  console.log(JSON.stringify(params, null, 2));
}

main();
