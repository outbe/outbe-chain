// E1 — full cross-chain auction demo runbook: orchestrator (QC-1261).
//
// Shared runtime for the phase tasks in tasks/demo/auction.ts: dual-chain viem clients (one key per
// chain from .env), artifact-ABI contract handles, the AuctionConfig builder, and the ERC20 approve
// ABI for the escrow lock. The runner is the operator on Outbe and the bidder on BNB.

import * as fs from "fs";
import {
  createPublicClient,
  createWalletClient,
  defineChain,
  getContract,
  http,
  type Address,
  type GetContractReturnType,
  type PublicClient,
  type WalletClient,
} from "viem";
import { privateKeyToAccount, type PrivateKeyAccount } from "viem/accounts";
import { bsc, bscTestnet } from "viem/chains";

import { type DemoNetwork, isOutbe } from "./harness/config.js";

const OUTBE_CHAIN_IDS: Record<string, number> = {
  outbeTestnet: 512215,
  outbeTestnetNew: 54322345,
  outbeDevnet: 424242,
  outbePrivnet: 512512,
};
const OUTBE_DEFAULT_RPC: Record<string, string> = {
  outbeTestnet: "https://eth.testnet.outbe.net",
  outbeTestnetNew: "https://rpc.testnet.outbe.net",
  outbeDevnet: "https://eth.d.outbe.net",
  outbePrivnet: "https://eth.p.outbe.net",
};

function chainFor(network: DemoNetwork) {
  if (network === "bscTestnet") return bscTestnet;
  if (network === "bsc") return bsc;
  return defineChain({
    id: OUTBE_CHAIN_IDS[network],
    name: network,
    nativeCurrency: { name: "OUT", symbol: "OUT", decimals: 18 },
    rpcUrls: { default: { http: [process.env.OUTBE_RPC_URL ?? OUTBE_DEFAULT_RPC[network]] } },
  });
}

export interface Runner {
  network: DemoNetwork;
  account: PrivateKeyAccount;
  publicClient: PublicClient;
  walletClient: WalletClient;
  /** Raw private key (needed by bidders.ts createCommitHash / createRevealSignature). */
  privateKey: `0x${string}`;
}

/** Build viem clients for `network` from .env (OUTBE_* on Outbe, BSC_TESTNET_* on BNB). */
export function getRunner(network: DemoNetwork): Runner {
  const pkEnv = isOutbe(network) ? "OUTBE_PRIVATE_KEY" : "BSC_TESTNET_PRIVATE_KEY";
  const rpcEnv = isOutbe(network) ? "OUTBE_RPC_URL" : "BSC_TESTNET_RPC_URL";
  const pk = process.env[pkEnv];
  if (!pk) throw new Error(`${pkEnv} required in .env for network ${network}`);
  const rpc = process.env[rpcEnv] ?? (isOutbe(network) ? OUTBE_DEFAULT_RPC[network] : undefined);
  if (!rpc) throw new Error(`${rpcEnv} required in .env for network ${network}`);

  const chain = chainFor(network);
  const privateKey = (pk.startsWith("0x") ? pk : `0x${pk}`) as `0x${string}`;
  const account = privateKeyToAccount(privateKey);
  const transport = http(rpc);
  const publicClient = createPublicClient({ chain, transport }) as PublicClient;
  const walletClient = createWalletClient({ account, chain, transport });
  return { network, account, publicClient, walletClient, privateKey };
}

const ARTIFACTS: Record<string, string> = {
  Desis: "artifacts/contracts/outbe/Desis.sol/Desis.json",
  IntexFactory: "artifacts/contracts/outbe/IntexFactory.sol/IntexFactory.json",
  OriginMessenger: "artifacts/contracts/outbe/OriginMessenger.sol/OriginMessenger.json",
  IntexAuction: "artifacts/contracts/bnb/IntexAuction.sol/IntexAuction.json",
  TargetMessenger: "artifacts/contracts/bnb/TargetMessenger.sol/TargetMessenger.json",
  EscrowAdapter: "artifacts/contracts/bnb/EscrowAdapter.sol/EscrowAdapter.json",
  IntexNFT1155: "artifacts/contracts/shared/IntexNFT1155.sol/IntexNFT1155.json",
  IntexSettlement: "artifacts/contracts/outbe/IntexSettlement.sol/IntexSettlement.json",
};

export function loadAbi(name: string): unknown[] {
  const p = ARTIFACTS[name];
  if (!p || !fs.existsSync(p)) throw new Error(`Artifact not found: ${name}. Run 'yarn compile' first.`);
  return (JSON.parse(fs.readFileSync(p, "utf-8")) as { abi: unknown[] }).abi;
}

/** A viem contract handle bound to the runner's public + wallet clients. Typed loosely (artifact ABI). */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function contractAt(runner: Runner, name: string, address: Address): GetContractReturnType<any, any> {
  return getContract({
    address,
    abi: loadAbi(name) as never,
    client: { public: runner.publicClient, wallet: runner.walletClient },
  });
}

/** Minimal ERC20 ABI for the escrow approve in the bid phase. */
export const ERC20_ABI = [
  {
    type: "function",
    name: "approve",
    stateMutability: "nonpayable",
    inputs: [
      { name: "spender", type: "address" },
      { name: "amount", type: "uint256" },
    ],
    outputs: [{ type: "bool" }],
  },
  {
    type: "function",
    name: "allowance",
    stateMutability: "view",
    inputs: [
      { name: "owner", type: "address" },
      { name: "spender", type: "address" },
    ],
    outputs: [{ type: "uint256" }],
  },
] as const;

/** AuctionConfig mirrors IDesis.AuctionConfig. clearingTimestamp and coenPriceFloor are derived on-chain. */
export interface AuctionConfig {
  seriesId: number;
  revealWindow: number;
  issuanceWindow: number;
  intexSize: bigint;
  minIntexBidPrice: bigint;
  intexStrikePrice: bigint;
  minIntexBidQuantity: number;
}

/** 12:00 UTC of the seriesId date (yyyymmdd). Used in demos for display; on-chain Desis derives it itself. */
export function clearingTimestampFor(seriesId: number): number {
  const s = String(seriesId);
  if (s.length !== 8) throw new Error(`seriesId must be yyyymmdd (got ${seriesId})`);
  const year = Number(s.slice(0, 4));
  const month = Number(s.slice(4, 6));
  const day = Number(s.slice(6, 8));
  return Math.floor(Date.UTC(year, month - 1, day, 12) / 1000);
}

/** Build an AuctionConfig with demo-friendly defaults; override via the task options. */
export function buildAuctionConfig(opts: { seriesId: number }): AuctionConfig {
  return {
    seriesId: opts.seriesId,
    revealWindow: 12 * 60 * 60,
    issuanceWindow: 60 * 60,
    intexSize: 1000n,
    minIntexBidPrice: 0n,
    intexStrikePrice: 2_800_000_000n,
    minIntexBidQuantity: 4,
  };
}

/** IssuanceConfig mirrors IDesis.IssuanceConfig. coenPriceCallTrigger is derived on-chain. */
export interface IssuanceConfig {
  intexCallPeriod: number;
  settlementTokenAlias: number;
  callWindowDays: number;
  callThresholdDays: number;
}

/** Build an IssuanceConfig with demo-friendly defaults. */
export function buildIssuanceConfig(): IssuanceConfig {
  return {
    intexCallPeriod: 0,
    settlementTokenAlias: 840,
    callWindowDays: 30,
    callThresholdDays: 20,
  };
}

/** intexStrikePrice * 1.08 / intexSize — matches Desis._derivedCoenPriceFloor. */
function derivedCoenPriceFloor(strikePrice: bigint, intexSize: bigint): bigint {
  return (strikePrice * 108n) / (intexSize * 100n);
}

/** Build the LZ quote-friendly start params; mirrors the derivation Desis does on-chain. */
export function auctionStageStartParams(config: AuctionConfig) {
  const clearing = clearingTimestampFor(config.seriesId);
  return {
    seriesId: config.seriesId,
    commitEnd: clearing - config.revealWindow,
    revealEnd: clearing,
    issuanceEnd: clearing + config.issuanceWindow,
    intexSize: config.intexSize,
    minIntexBidPrice: config.minIntexBidPrice,
    intexStrikePrice: config.intexStrikePrice,
    coenPriceFloor: derivedCoenPriceFloor(config.intexStrikePrice, config.intexSize),
    minIntexBidQuantity: config.minIntexBidQuantity,
  };
}

/** Read the `nativeFee` field from an `OriginMessenger`/`TargetMessenger` quote. */
export type MessagingFee = { nativeFee: bigint; lzTokenFee: bigint };
export const nativeFee = (fee: MessagingFee): bigint => fee.nativeFee;

/** BNB chain id used for the bid's EIP-712-bound chainId arg (revealBid requires chainId == block.chainid). */
export const bnbChainId = (network: DemoNetwork): bigint => BigInt(chainFor(network).id);

/** Desis AuctionStage enum (mirror IDesis). */
export const AUCTION_STAGE = ["None", "Started", "Revealing", "BidsReceived", "Cleared", "Cancelled"] as const;
