import type { HardhatUserConfig } from "hardhat/config";

import hardhatToolboxViemPlugin from "@nomicfoundation/hardhat-toolbox-viem";
import hardhatTypechain from "@nomicfoundation/hardhat-typechain";
import hardhatVerify from "@nomicfoundation/hardhat-verify";
import { configVariable } from "hardhat/config";
import "dotenv/config";
import { generateCommitHashTasks } from "../tasks/auction/generateCommitHash.js";
import { qualifiedFlowTasks } from "../tasks/intex/qualified.js";
import { lzBridgeUtilTasks } from "../tasks/layerzero/bridgeUtils.js";
import { lzNonceClearTasks } from "../tasks/layerzero/nonceClear.js";
import { onft1155Tasks } from "../tasks/layerzero/onft1155Transfer.js";
import { wireTasks } from "../tasks/cd/wire.js";
import { demoTasks } from "../tasks/demo/index.js";

const config: HardhatUserConfig = {
  plugins: [hardhatToolboxViemPlugin, hardhatTypechain, hardhatVerify],
  tasks: [...generateCommitHashTasks, ...qualifiedFlowTasks, ...lzBridgeUtilTasks, ...lzNonceClearTasks, ...onft1155Tasks, ...wireTasks, ...demoTasks],
  paths: {
    sources: ["./contracts", "./test/mocks"],
    tests: "./test/hardhat",
  },
  solidity: {
    npmFilesToBuild: ["@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol"],
    profiles: {
      default: {
        version: "0.8.30",
        settings: {
          evmVersion: "prague",
          optimizer: {
            enabled: true,
            runs: 200,
          },
          viaIR: true
        }
      },
      production: {
        version: "0.8.30",
        settings: {
          evmVersion: "prague",
          optimizer: {
            enabled: true,
            runs: 200,
          },
          viaIR: true
        },
      },
    },
  },
  verify: {
    etherscan: {
      apiKey: configVariable("ETHERSCAN_API_KEY"),
    }
  },
  networks: {
    default: {
      type: "edr-simulated",
      chainType: "l1",
      allowUnlimitedContractSize: true,
    },
    hardhatMainnet: {
      type: "edr-simulated",
      chainType: "l1",
      allowUnlimitedContractSize: true,
    },
    hardhatOp: {
      type: "edr-simulated",
      chainType: "op",
      allowUnlimitedContractSize: true,
    },
    sepolia: {
      type: "http",
      chainType: "l1",
      url: configVariable("SEPOLIA_RPC_URL"),
      accounts: [configVariable("SEPOLIA_PRIVATE_KEY")],
    },
    bscTestnet: {
      type: "http",
      chainType: "l1",
      url: configVariable("BSC_TESTNET_RPC_URL"),
      accounts: [configVariable("BSC_TESTNET_PRIVATE_KEY")],
      chainId: 97,
    },
    bsc: {
      type: "http",
      chainType: "l1",
      url: configVariable("BSC_MAINNET_RPC_URL"),
      accounts: [configVariable("BSC_MAINNET_PRIVATE_KEY")],
      chainId: 56,
    },
    base: {
      type: "http",
      url: configVariable("BASE_RPC_URL"),
      accounts: [configVariable("BASE_PRIVATE_KEY")],
      chainId: 8453,
    },
    baseSepolia: {
      type: "http",
      url: configVariable("BASE_SEPOLIA_RPC_URL"),
      accounts: [configVariable("BASE_SEPOLIA_PRIVATE_KEY")],
      chainId: 84532,
    },
    outbePrivnet: {
      type: "http",
      url: "https://eth.p.outbe.net",
      accounts: [configVariable("OUTBE_PRIVATE_KEY")],
      chainId: 512512,
    },
    outbeDevnet: {
      type: "http",
      url: "https://eth.d.outbe.net",
      accounts: [configVariable("OUTBE_PRIVATE_KEY")],
      chainId: 424242,
    },
    outbeTestnet: {
      type: "http",
      url: "https://eth.testnet.outbe.net",
      accounts: [configVariable("OUTBE_PRIVATE_KEY")],
      chainId: 512215,
    },
    outbeTestnetNew: {
      type: "http",
      url: "https://rpc.testnet.outbe.net",
      accounts: [configVariable("OUTBE_PRIVATE_KEY")],
      chainId: 54322345,
    },
  },
};

export default config;
