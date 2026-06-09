// Shared Runtime Utilities
// Creates runtime objects for auction and bidder operations.
// Handles contract discovery from Ignition deployments.

import path from "path";
import { existsSync, readFileSync } from "fs";
import { createPublicClient, createWalletClient, getContract, http } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { bsc, bscTestnet } from "viem/chains";
import type { Hex } from "viem";
import { OUTBE_CHAINS } from "./layerzero.js";
import { getNetworkName } from "./taskUtils.js";
import type { AuctionRuntime, AuctionContract } from "../auction/flow.js";
import type { AuctionBidderRuntime, AuctionBidderContract } from "../auction/bidders.js";
import type { Intex1155IssuanceRuntime } from "../intex/issuance.js";
import type { HardhatRuntimeEnvironmentLike, ViemNetworkLike } from "./types.js";

// =============================================================================
// Contract Discovery
// =============================================================================

/**
 * Find deployed Auction contract address from Ignition deployments.
 * Throws if not found.
 */
export async function findDeployedAuctionAddress(viem: ViemNetworkLike): Promise<Hex> {
  const publicClient = await viem.getPublicClient();
  const chainId = Number(await publicClient.getChainId());

  const jsonPath = path.join(
    process.cwd(),
    "ignition",
    "deployments",
    `chain-${chainId}`,
    "deployed_addresses.json",
  );

  if (!existsSync(jsonPath)) {
    throw new Error(
      `deployed_addresses.json not found for chain ${chainId}. Provide --address or deploy first.`,
    );
  }

  const data = JSON.parse(readFileSync(jsonPath, "utf8")) as Record<string, string>;
  const addr = data["IntexAuctionModule#IntexAuction"];

  if (!addr) {
    throw new Error(`IntexAuctionModule#IntexAuction not found in ${jsonPath}`);
  }

  return addr as Hex;
}

/**
 * Find deployed IntexNFT1155 contract address from Ignition deployments.
 * Throws if not found.
 */
export async function findDeployedIntex1155Address(viem: ViemNetworkLike): Promise<Hex> {
  const publicClient = await viem.getPublicClient();
  const chainId = Number(await publicClient.getChainId());

  const jsonPath = path.join(
    process.cwd(),
    "ignition",
    "deployments",
    `chain-${chainId}`,
    "deployed_addresses.json",
  );

  if (!existsSync(jsonPath)) {
    throw new Error(
      `deployed_addresses.json not found for chain ${chainId}. Provide --intex-address or deploy first.`,
    );
  }

  const data = JSON.parse(readFileSync(jsonPath, "utf8")) as Record<string, string>;
  const addr = data["IntexNFT1155Module#intex1155"];

  if (!addr) {
    throw new Error(`IntexNFT1155Module#intex1155 not found in ${jsonPath}`);
  }

  return addr as Hex;
}

// =============================================================================
// Runtime Factories
// =============================================================================

/**
 * Create AuctionRuntime for auction flow operations.
 */
export async function createAuctionRuntime(
  hre: unknown,
  addressOverride?: string,
): Promise<AuctionRuntime> {
  const hreTyped = hre as HardhatRuntimeEnvironmentLike;
  const { viem } = await hreTyped.network.connect();
  const publicClient = await viem.getPublicClient();
  const [wallet] = await viem.getWalletClients();

  const address = (addressOverride ?? (await findDeployedAuctionAddress(viem))) as Hex;
  const contractRaw = await viem.getContractAt("IntexAuction", address);

  const contract = contractRaw as unknown as {
    read: AuctionContract["read"];
    write: AuctionContract["write"];
  };

  return {
    address,
    contract: {
      read: contract.read,
      write: contract.write,
    },
    viem: {
      getContractAt: async (abi: string | readonly unknown[], addr: `0x${string}`) => {
        return await viem.getContractAt(abi as string, addr);
      },
      getPublicClient: async () => publicClient,
      getWalletClients: async () => [wallet],
    },
    publicClient: {
      waitForTransactionReceipt: async (args: { hash: Hex }) => {
        await publicClient.waitForTransactionReceipt(args);
      },
    },
    wallet: {
      account: wallet.account,
    },
  };
}

type BiddersNetworkId = "bscTestnet" | "bsc";

const BIDDERS_NETWORK_CONFIG: Record<
  BiddersNetworkId,
  { chain: typeof bscTestnet | typeof bsc; rpcEnv: string; pkEnv: string; defaultRpc: string }
> = {
  bscTestnet: {
    chain: bscTestnet,
    rpcEnv: "BSC_TESTNET_RPC_URL",
    pkEnv: "BSC_TESTNET_PRIVATE_KEY",
    defaultRpc: "https://bsc-testnet.publicnode.com",
  },
  bsc: {
    chain: bsc,
    rpcEnv: "BSC_MAINNET_RPC_URL",
    pkEnv: "BSC_MAINNET_PRIVATE_KEY",
    defaultRpc: "https://bsc-dataseed1.binance.org",
  },
};

/**
 * Create AuctionBidderRuntime for bidder operations.
 * @param opts.networkForBidders - Network where Auction lives (e.g. bscTestnet when task runs on outbeDevnet).
 */
export async function createBidderRuntime(
  hre: unknown,
  addressOverride?: string,
  opts?: { networkForBidders?: string },
): Promise<AuctionBidderRuntime> {
  const hreTyped = hre as HardhatRuntimeEnvironmentLike & { network?: { name?: string }; artifacts?: { readArtifact: (name: string) => Promise<{ abi: unknown[] }> } };
  const networkName = getNetworkName(hre);
  const biddersNet = opts?.networkForBidders as BiddersNetworkId | undefined;
  const config = biddersNet && BIDDERS_NETWORK_CONFIG[biddersNet];

  let viem: ViemNetworkLike;
  let chainId: bigint;

  const useOverride =
    config && (networkName in OUTBE_CHAINS || biddersNet !== networkName);

  if (useOverride) {
    const rpc = process.env[config.rpcEnv] ?? config.defaultRpc;
    const pk = process.env[config.pkEnv];
    if (!pk) throw new Error(`${config.pkEnv} required when running bidders on ${biddersNet}`);
    const account = privateKeyToAccount(pk as `0x${string}`);
    const transport = http(rpc);
    const publicClient = createPublicClient({ chain: config.chain, transport });
    const walletClient = createWalletClient({ account, chain: config.chain, transport });
    chainId = BigInt(config.chain.id);
    const artifacts = hreTyped.artifacts!;
    viem = {
      getContractAt: async (name: string, addr: Hex) => {
        const { abi } = await artifacts.readArtifact(name);
        return getContract({ address: addr, abi, client: { public: publicClient, wallet: walletClient } });
      },
      getPublicClient: async () => publicClient,
      getWalletClients: async () => [walletClient],
    } as unknown as ViemNetworkLike;
  } else {
    const connected = await hreTyped.network.connect();
    viem = connected.viem;
    const publicClient = await viem.getPublicClient();
    chainId = await publicClient.getChainId();
  }

  const pubClient = await viem.getPublicClient();
  const address = (addressOverride ?? (await findDeployedAuctionAddress(viem))) as Hex;
  const contractRaw = await viem.getContractAt("IntexAuction", address);

  const contract = contractRaw as unknown as {
    read: AuctionBidderContract["read"];
    write: AuctionBidderContract["write"];
  };

  return {
    address,
    contract: {
      read: {
        getAuctionInfo: contract.read.getAuctionInfo.bind(contract.read),
        getAuctionStage: contract.read.getAuctionStage.bind(contract.read),
      },
      write: {
        commitBid: contract.write.commitBid.bind(contract.write),
        revealBid: contract.write.revealBid.bind(contract.write),
      },
    },
    viem: {
      getContractAt: async (abi: string | readonly unknown[], addr: `0x${string}`) => {
        return await viem.getContractAt(abi as string, addr);
      },
      getPublicClient: async () => pubClient,
      getWalletClients: async () => await viem.getWalletClients(),
    },
    publicClient: {
      waitForTransactionReceipt: async (args: { hash: Hex }) => {
        const receipt = await pubClient.waitForTransactionReceipt(args) as unknown as {
          status: "success" | "reverted";
          transactionHash: Hex;
          blockNumber: bigint;
        };
        return receipt;
      },
      getChainId: async () => Number(chainId),
    },
    chainId: Number(chainId),
  };
}

/**
 * Create runtime for IntexNFT1155 issuance operations.
 */
export async function createIntex1155IssuanceRuntime(
  hre: unknown,
  opts?: { auctionAddress?: string; intexAddress?: string },
): Promise<Intex1155IssuanceRuntime> {
  const hreTyped = hre as HardhatRuntimeEnvironmentLike;
  const { viem } = await hreTyped.network.connect();
  const publicClient = await viem.getPublicClient();
  const [wallet] = await viem.getWalletClients();

  const auctionAddress = (opts?.auctionAddress ?? (await findDeployedAuctionAddress(viem))) as Hex;
  const intexAddress = (opts?.intexAddress ?? (await findDeployedIntex1155Address(viem))) as Hex;

  const auctionRaw = await viem.getContractAt("IntexAuction", auctionAddress);
  const intexRaw = await viem.getContractAt("IntexNFT1155", intexAddress);

  const auction = auctionRaw as unknown as {
    read: {
      getAuctionDetails: (a: [number]) => Promise<unknown>;
    };
  };

  const intex = intexRaw as unknown as {
    write: {
      createSeries: (
        a: [
          number,
          number,
          bigint,
          bigint,
          bigint,
          number,
          number,
          { windowDays: number; thresholdDays: number; coenPriceCallTrigger: bigint },
        ],
        o: { account: { address: Hex } },
      ) => Promise<Hex>;
      mintBatch: (
        a: [readonly `0x${string}`[], bigint[], number],
        o: { account: { address: Hex } },
      ) => Promise<Hex>;
    };
  };

  return {
    auctionRead: {
      getAuctionDetails: auction.read.getAuctionDetails.bind(
        auction.read,
      ) as Intex1155IssuanceRuntime["auctionRead"]["getAuctionDetails"],
    },
    intex1155Write: {
      createSeries: intex.write.createSeries.bind(intex.write),
      mintBatch: intex.write.mintBatch.bind(intex.write),
    },
    publicClient: {
      waitForTransactionReceipt: async (args: { hash: Hex }) => {
        await publicClient.waitForTransactionReceipt(args);
      },
    },
    wallet: { account: wallet!.account },
  };
}
