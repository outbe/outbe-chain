// Cross-chain Auction Flow Script
// Manages the Outbe-side auction lifecycle:
// - Telosis: auction creation, clearing, issuance/refund instructions, markSeriesCalled
//
// Flow:
// 1. Create and start auction (Telosis + BNB bridge message)
// 2. Send reveal stage to BNB
// 3. Send clearing stage to BNB
// 4. Clear auction (after bids received from BNB)
// 5. Send issuance instructions to BNB
// 6. Send refund instructions to BNB

import type { Address, Hex, PublicClient as ViemPublicClient, WalletClient as ViemWalletClient } from "viem";
import { createPublicClient, createWalletClient, formatEther, getContract, http, parseEther } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { bsc, bscTestnet } from "viem/chains";
import { normalizeSeries, seriesIdToNoonTimestamp, seriesIdToUint32 } from "../shared/auctionId.js";
import { addressToBytes32, ENDPOINT_NONCE_ABI, LZ_INFRA, NETWORK_TO_EID, OUTBE_CHAINS } from "../shared/layerzero.js";
import { getNetworkName } from "../shared/taskUtils.js";

// =============================================================================
// Types
// =============================================================================

// Wallet account for contract interactions
export interface WalletAccount {
  address: `0x${string}`;
}

// Public client interface
export interface PublicClient {
  waitForTransactionReceipt(args: { hash: `0x${string}` }): Promise<void>;
}

// Viem client interface
export interface ViemClient {
  getContractAt(abi: string | readonly unknown[], address: Address): Promise<unknown>;
  getPublicClient(): Promise<ViemPublicClient>;
  getWalletClients(): Promise<readonly ViemWalletClient[]>;
}

// AuctionConfig for Telosis.createAndStartAuction
// Mirrors the Solidity AuctionConfig struct in contracts/outbe/interfaces/IDesis.sol.
// Field order MUST match the struct — viem encodes by position when invoking the contract.
export interface AuctionConfig {
  seriesId: number;
  clearingTimestamp: number;
  promisLoadMinor: bigint;
  minIntexBidPrice: bigint;
  costAmountMinor: bigint;
  floorPriceMinor: bigint;
  minIntexBidQuantity: number;
  /** Call period in seconds; 0 ⇒ contract default of 21 days (used at issuance). */
  intexCallPeriod: number;
  /** ISO 4217 numeric alias of the settlement token (840 = USD). */
  settlementTokenAlias: number;
  /** Forced-call observation window length in days. */
  callWindowDays: number;
  /** Number of days within the window that breach the forced-call trigger. */
  callThresholdDays: number;
  /** COEN price level that arms the forced call. */
  callPriceMinor: bigint;
}

// Telosis contract read API
export interface TelosisReadApi {
  getAuctionConfig(args: [number]): Promise<AuctionConfig>;
  getAuctionStage(args: [number]): Promise<number>;
  bridgeAdapter(): Promise<Address>;
  getBids(args: [number]): Promise<readonly BidData[]>;
  getBidsCount(args: [number]): Promise<bigint>;
  getClearingResult(args: [number]): Promise<ClearingResult>;
}

// Telosis contract write API
export interface TelosisWriteApi {
  createAndStartAuction(
    args: [AuctionConfig, Hex],
    opts: { account: WalletAccount; value: bigint },
  ): Promise<`0x${string}`>;
  sendRevealStage(
    args: [number, boolean, Hex],
    opts: { account: WalletAccount; value: bigint },
  ): Promise<`0x${string}`>;
  sendClearingStage(
    args: [number, Hex],
    opts: { account: WalletAccount; value: bigint },
  ): Promise<`0x${string}`>;
  clearAuction(
    args: [number, number, Hex],
    opts: { account: WalletAccount; value: bigint },
  ): Promise<`0x${string}`>;
  sendIssuanceInstructions(
    args: [number, Hex],
    opts: { account: WalletAccount; value: bigint },
  ): Promise<`0x${string}`>;
  sendRefundInstructions(
    args: [number, Hex],
    opts: { account: WalletAccount; value: bigint },
  ): Promise<`0x${string}`>;
}

// BidData from Telosis — mirrors the on-chain IDesis.BidData struct
export interface BidData {
  bidderAddress: Address;
  intexBidPrice: bigint;
  timestamp: number;
  intexQuantity: number;
}

// ClearingResult from Telosis — mirrors the on-chain IDesis.ClearingResult struct
export interface ClearingResult {
  issuedIntexCount: number;
  clearingPrice: bigint;
  winners: readonly Address[];
  winnerQuantities: readonly bigint[];
  allBidders: readonly Address[];
  refundedAmounts: readonly bigint[];
  paidAmounts: readonly bigint[];
}

// Telosis contract interface
export interface TelosisContract {
  read: TelosisReadApi;
  write: TelosisWriteApi;
}

// Runtime context for Outbe operations
export interface OutbeRuntime {
  telosisAddress: Address;
  telosis: TelosisContract;
  viem: ViemClient;
  publicClient: PublicClient;
  wallet: { account: WalletAccount };
}

// Hardhat Runtime Environment interface
export interface HardhatRuntimeEnvironmentLike {
  network: {
    connect(): Promise<{
      viem: {
        getPublicClient(): Promise<ViemPublicClient>;
        getWalletClients(): Promise<readonly ViemWalletClient[]>;
        getContractAt(name: string, address: Address): Promise<unknown>;
      };
    }>;
  };
}

// =============================================================================
// Argument Types
// =============================================================================

type BigIntInput = string | number | bigint | undefined;

export interface OutbeFlowArgs {
  series?: string;
  floor?: BigIntInput;
  clearingTimestamp?: BigIntInput;
  promisLoadMinor?: BigIntInput;
  costAmountMinor?: BigIntInput;
  floorPriceMinor?: BigIntInput;
  minIntexBidQuantity?: BigIntInput;
  supply?: BigIntInput;
  extraOptions?: Hex;
  msgValue?: BigIntInput;
}

export interface CreateAuctionArgs {
  series?: string;
  floor?: BigIntInput;
  clearingTimestamp?: BigIntInput;
  promisLoadMinor?: BigIntInput;
  costAmountMinor?: BigIntInput;
  floorPriceMinor?: BigIntInput;
  minIntexBidQuantity?: BigIntInput;
  intexCallPeriod?: BigIntInput;
  settlementTokenAlias?: BigIntInput;
  callWindowDays?: BigIntInput;
  callThresholdDays?: BigIntInput;
  callPriceMinor?: BigIntInput;
  extraOptions?: Hex;
  msgValue?: BigIntInput;
}

export interface SendRevealStageArgs {
  seriesId: number;
  isGreenDay?: boolean;
  extraOptions?: Hex;
  msgValue?: BigIntInput;
}

export interface SendClearingStageArgs {
  seriesId: number;
  extraOptions?: Hex;
  msgValue?: BigIntInput;
}

export interface ClearAuctionArgs {
  seriesId: number;
  supply?: BigIntInput;
  extraOptions?: Hex;
  msgValue?: BigIntInput;
}

export interface SendIssuanceInstructionsArgs {
  seriesId: number;
  extraOptions?: Hex;
  msgValue?: BigIntInput;
}

export interface SendRefundInstructionsArgs {
  seriesId: number;
  extraOptions?: Hex;
  msgValue?: BigIntInput;
}

// =============================================================================
// Constants
// =============================================================================

const DEFAULT_FLOOR = 80_000_000n; // 80 payment-token units (6 decimals)
const DEFAULT_PROMIS_LOAD_MINOR = 1_000_000_000_000_000_000_000_000n; // 1M Promis (18 decimals)
const DEFAULT_COST_AMOUNT_MINOR = 1_000_000_000n;
const DEFAULT_FLOOR_PRICE_MINOR = 80_000_000n;
const DEFAULT_MIN_INTEX_BID_QUANTITY = 5;
const DEFAULT_INTEX_CALL_PERIOD = 0; // 0 ⇒ contract default of 21 days
const DEFAULT_SETTLEMENT_TOKEN_ALIAS = 840; // ISO 4217 numeric alias (840 = USD)
const DEFAULT_CALL_WINDOW_DAYS = 0;
const DEFAULT_CALL_THRESHOLD_DAYS = 0;
const DEFAULT_CALL_PRICE_MINOR = 0n;
const DEFAULT_MSG_VALUE = 100_000_000_000_000_000n; // 0.1 COEN fallback (LZ Outbe->BSC often ~0.13 COEN)
const FEE_BUFFER_BPS = 50n; // 0.5% buffer over quoted fee
const EMPTY_EXTRA_OPTIONS = "0x" as Hex;

// =============================================================================
// Utilities
// =============================================================================

// Convert input to bigint, return undefined for empty/undefined
function toOptionalBigInt(input: BigIntInput): bigint | undefined {
  if (input === undefined) return undefined;
  if (typeof input === "bigint") return input;
  if (typeof input === "number") return BigInt(input);
  const trimmed = String(input).trim();
  return trimmed === "" ? undefined : BigInt(trimmed);
}

// Convert input to number, return undefined for empty/undefined
function toOptionalNumber(input: BigIntInput): number | undefined {
  const b = toOptionalBigInt(input);
  return b === undefined ? undefined : Number(b);
}

// Parse floor price with default
function parseFloor(floor?: BigIntInput): bigint {
  return toOptionalBigInt(floor) ?? DEFAULT_FLOOR;
}

// Quote LayerZero fee from bridge adapter, add buffer. Returns 0n if quote fails (use fallback).
// Fee can be { nativeFee, lzTokenFee } or [nativeFee, lzTokenFee] (viem tuple).
async function quoteAndBuffer(
  _runtime: OutbeRuntime,
  getFee: () => Promise<unknown>,
): Promise<bigint> {
  try {
    const fee = await getFee();
    const nativeFee =
      fee != null && typeof fee === "object" && "nativeFee" in fee && typeof (fee as { nativeFee: unknown }).nativeFee === "bigint"
        ? (fee as { nativeFee: bigint }).nativeFee
        : Array.isArray(fee) && typeof fee[0] === "bigint"
          ? fee[0]
          : fee != null && typeof fee === "object" && 0 in fee && typeof (fee as { 0: unknown })[0] === "bigint"
            ? (fee as { 0: bigint })[0]
            : undefined;
    if (nativeFee == null) {
      console.warn("[quote] Invalid fee structure (nativeFee missing), using default");
      return 0n;
    }
    return nativeFee + (nativeFee * FEE_BUFFER_BPS) / 10000n;
  } catch (e) {
    console.warn("[quote] Failed to quote LZ fee, using default:", (e as Error).message);
    return 0n;
  }
}

// =============================================================================
// Runtime Factory
// =============================================================================

// Create OutbeRuntime for contract operations.
export async function createOutbeRuntime(
  hre: unknown,
  opts: {
    telosisAddress: string;
  },
): Promise<OutbeRuntime> {
  const networkName = getNetworkName(hre);
  let viem: {
    getContractAt: (name: string, address: Address) => Promise<unknown>;
    getPublicClient: () => Promise<ViemPublicClient>;
    getWalletClients: () => Promise<readonly ViemWalletClient[]>;
  };
  let publicClient: ViemPublicClient;
  let wallet: ViemWalletClient;

  if (networkName in OUTBE_CHAINS) {
    const chain = OUTBE_CHAINS[networkName as keyof typeof OUTBE_CHAINS];
    const rpc = process.env.OUTBE_RPC_URL ?? chain.rpcUrls.default.http[0];
    const pk = process.env.OUTBE_PRIVATE_KEY;
    if (!pk) throw new Error("OUTBE_PRIVATE_KEY required for Outbe networks");
    const account = privateKeyToAccount(pk as `0x${string}`);
    const transport = http(rpc);
    publicClient = createPublicClient({ chain, transport });
    wallet = createWalletClient({ account, chain, transport }) as ViemWalletClient;
    const artifacts = (hre as { artifacts: { readArtifact: (name: string) => Promise<{ abi: unknown[] }> } }).artifacts;
    viem = {
      getContractAt: async (name: string, address: Address) => {
        const { abi } = await artifacts.readArtifact(name);
        return getContract({ address, abi, client: { public: publicClient, wallet } });
      },
      getPublicClient: async () => publicClient,
      getWalletClients: async () => [wallet],
    };
  } else {
    const hreTyped = hre as HardhatRuntimeEnvironmentLike;
    const connected = await hreTyped.network.connect();
    viem = connected.viem;
    publicClient = await viem.getPublicClient();
    const [w] = await viem.getWalletClients();
    wallet = w!;
  }

  const telosisAddress = opts.telosisAddress as Address;
  const telosisRaw = await viem.getContractAt("Desis", telosisAddress);
  const telosis = telosisRaw as unknown as TelosisContract;

  if (!wallet.account) {
    throw new Error("No wallet account available. Check your network configuration.");
  }

  const runtime: OutbeRuntime = {
    telosisAddress,
    telosis,
    viem: {
      getContractAt: async (abi: string | readonly unknown[], addr: Address) => {
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
      account: wallet.account as WalletAccount,
    },
  };

  return runtime;
}

// =============================================================================
// Auction Lifecycle Operations
// =============================================================================

// Create and start auction on Telosis (sends LZ message to BNB in a single tx).
export async function createAndStartAuction(
  runtime: OutbeRuntime,
  args: CreateAuctionArgs,
): Promise<{ seriesId: number }> {
  const { telosis, publicClient, wallet } = runtime;

  const seriesStr = normalizeSeries(args.series);
  const seriesId = seriesIdToUint32(seriesStr);

  const minIntexBidPrice = parseFloor(args.floor);
  const clearingTimestamp =
    toOptionalNumber(args.clearingTimestamp) ?? Number(seriesIdToNoonTimestamp(seriesStr));
  const promisLoadMinor = toOptionalBigInt(args.promisLoadMinor) ?? DEFAULT_PROMIS_LOAD_MINOR;
  const costAmountMinor = toOptionalBigInt(args.costAmountMinor) ?? DEFAULT_COST_AMOUNT_MINOR;
  const floorPriceMinor = toOptionalBigInt(args.floorPriceMinor) ?? DEFAULT_FLOOR_PRICE_MINOR;
  const minIntexBidQuantity =
    toOptionalNumber(args.minIntexBidQuantity) ?? DEFAULT_MIN_INTEX_BID_QUANTITY;
  const intexCallPeriod = toOptionalNumber(args.intexCallPeriod) ?? DEFAULT_INTEX_CALL_PERIOD;
  const settlementTokenAlias =
    toOptionalNumber(args.settlementTokenAlias) ?? DEFAULT_SETTLEMENT_TOKEN_ALIAS;
  const callWindowDays = toOptionalNumber(args.callWindowDays) ?? DEFAULT_CALL_WINDOW_DAYS;
  const callThresholdDays = toOptionalNumber(args.callThresholdDays) ?? DEFAULT_CALL_THRESHOLD_DAYS;
  const callPriceMinor =
    toOptionalBigInt(args.callPriceMinor) ?? DEFAULT_CALL_PRICE_MINOR;

  const config: AuctionConfig = {
    seriesId,
    clearingTimestamp,
    promisLoadMinor,
    minIntexBidPrice,
    costAmountMinor,
    floorPriceMinor,
    minIntexBidQuantity,
    intexCallPeriod,
    settlementTokenAlias,
    callWindowDays,
    callThresholdDays,
    callPriceMinor,
  };

  const extraOptions = args.extraOptions ?? EMPTY_EXTRA_OPTIONS;
  let msgValue = toOptionalBigInt(args.msgValue);
  if (msgValue == null || msgValue === 0n) {
    const bridgeAddr = (await telosis.read.bridgeAdapter()) as Address;
    const bridge = (await runtime.viem.getContractAt("OriginMessenger", bridgeAddr)) as {
      read: {
        quoteSendAuctionStageStart: (args: unknown[]) => Promise<unknown>;
      };
    };
    const quoted = await quoteAndBuffer(runtime, () =>
      bridge.read.quoteSendAuctionStageStart([
        {
          seriesId,
          commitEnd: clearingTimestamp,
          revealEnd: clearingTimestamp,
          issuanceEnd: clearingTimestamp,
          promisLoadMinor,
          minIntexBidPrice,
          costAmountMinor,
          floorPriceMinor,
          minIntexBidQuantity,
        },
        extraOptions,
        false,
      ]),
    );
    msgValue = quoted > 0n ? quoted : DEFAULT_MSG_VALUE;
    if (quoted > 0n) console.log("[create-and-start] quoted fee:", quoted.toString(), "wei");
  }

  console.log("[create-and-start]", {
    seriesId,
    clearingTimestamp,
    minIntexBidPrice: minIntexBidPrice.toString(),
    promisLoadMinor: promisLoadMinor.toString(),
    costAmountMinor: costAmountMinor.toString(),
    floorPriceMinor: floorPriceMinor.toString(),
    minIntexBidQuantity,
    msgValue: msgValue.toString(),
  });

  const tx = await telosis.write.createAndStartAuction(
    [config, extraOptions],
    { account: wallet.account, value: msgValue },
  );
  await publicClient.waitForTransactionReceipt({ hash: tx });
  console.log("[create-and-start] done tx:", tx);

  return { seriesId };
}

// Send reveal stage to BNB via bridge message.
export async function sendRevealStage(
  runtime: OutbeRuntime,
  args: SendRevealStageArgs,
): Promise<void> {
  const { telosis, publicClient, wallet } = runtime;

  const isGreenDay = args.isGreenDay ?? true;
  const extraOptions = args.extraOptions ?? EMPTY_EXTRA_OPTIONS;
  let msgValue = toOptionalBigInt(args.msgValue);
  if (msgValue == null || msgValue === 0n) {
    const bridgeAddr = (await telosis.read.bridgeAdapter()) as Address;
    const bridge = (await runtime.viem.getContractAt("OriginMessenger", bridgeAddr)) as {
      read: { quoteSendAuctionStageReveal: (args: unknown[]) => Promise<{ nativeFee: bigint }> };
    };
    const quoted = await quoteAndBuffer(runtime, () =>
      bridge.read.quoteSendAuctionStageReveal([args.seriesId, isGreenDay, extraOptions, false]),
    );
    msgValue = quoted > 0n ? quoted : DEFAULT_MSG_VALUE;
    if (quoted > 0n) console.log("[send-reveal-stage] quoted fee:", quoted.toString(), "wei");
  }

  console.log("[send-reveal-stage]", {
    seriesId: args.seriesId,
    isGreenDay,
    msgValue: msgValue.toString(),
  });

  const tx = await telosis.write.sendRevealStage(
    [args.seriesId, isGreenDay, extraOptions],
    { account: wallet.account, value: msgValue },
  );
  await publicClient.waitForTransactionReceipt({ hash: tx });
  console.log("[send-reveal-stage] done tx:", tx);
}

// Send clearing stage to BNB via bridge message.
export async function sendClearingStage(
  runtime: OutbeRuntime,
  args: SendClearingStageArgs,
): Promise<void> {
  const { telosis, publicClient, wallet } = runtime;

  const extraOptions = args.extraOptions ?? EMPTY_EXTRA_OPTIONS;
  let msgValue = toOptionalBigInt(args.msgValue);
  if (msgValue == null || msgValue === 0n) {
    const bridgeAddr = (await telosis.read.bridgeAdapter()) as Address;
    const bridge = (await runtime.viem.getContractAt("OriginMessenger", bridgeAddr)) as {
      read: { quoteSendAuctionStageClearing: (args: unknown[]) => Promise<{ nativeFee: bigint }> };
    };
    const quoted = await quoteAndBuffer(runtime, () =>
      bridge.read.quoteSendAuctionStageClearing([args.seriesId, extraOptions, false]),
    );
    msgValue = quoted > 0n ? quoted : DEFAULT_MSG_VALUE;
    if (quoted > 0n) console.log("[send-clearing-stage] quoted fee:", quoted.toString(), "wei");
  }

  console.log("[send-clearing-stage]", {
    seriesId: args.seriesId,
    msgValue: msgValue.toString(),
  });

  const tx = await telosis.write.sendClearingStage(
    [args.seriesId, extraOptions],
    { account: wallet.account, value: msgValue },
  );
  await publicClient.waitForTransactionReceipt({ hash: tx });
  console.log("[send-clearing-stage] done tx:", tx);
}

// Clear auction on Telosis.
// Calculates clearing price and sends result to BNB.
// supply: INTEX supply to issue (mandatory, must be > 0).
export async function clearAuction(
  runtime: OutbeRuntime,
  args: ClearAuctionArgs,
): Promise<ClearingResult> {
  const { telosis, publicClient, wallet } = runtime;

  const supply = toOptionalNumber(args.supply) ?? 0;
  const extraOptions = args.extraOptions ?? EMPTY_EXTRA_OPTIONS;
  let msgValue = toOptionalBigInt(args.msgValue);
  if (msgValue == null || msgValue === 0n) {
    const bridgeAddr = (await telosis.read.bridgeAdapter()) as Address;
    const bridge = (await runtime.viem.getContractAt("OriginMessenger", bridgeAddr)) as {
      read: { quoteSendAuctionResult: (args: unknown[]) => Promise<{ nativeFee: bigint }> };
    };
    const cfg = await telosis.read.getAuctionConfig([args.seriesId]);
    const estIssued = supply > 0 ? supply : 100;
    const quoted = await quoteAndBuffer(runtime, () =>
      bridge.read.quoteSendAuctionResult([
        args.seriesId,
        estIssued,
        cfg.minIntexBidPrice,
        0,
        extraOptions,
        false,
      ]),
    );
    msgValue = quoted > 0n ? quoted : DEFAULT_MSG_VALUE;
    if (quoted > 0n) console.log("[clear-auction] quoted fee:", quoted.toString(), "wei");
  }

  const bidsCount = await telosis.read.getBidsCount([args.seriesId]);
  console.log("[clear-auction]", {
    seriesId: args.seriesId,
    supply,
    bidsCount: bidsCount.toString(),
    msgValue: msgValue.toString(),
  });

  const tx = await telosis.write.clearAuction(
    [args.seriesId, supply, extraOptions],
    { account: wallet.account, value: msgValue },
  );
  await publicClient.waitForTransactionReceipt({ hash: tx });
  console.log("[clear-auction] done tx:", tx);

  const result = await telosis.read.getClearingResult([args.seriesId]);
  console.log("[clear-auction] result:", {
    issuedIntexCount: result.issuedIntexCount.toString(),
    clearingPrice: result.clearingPrice.toString(),
    winnersCount: result.winners.length,
  });

  return result;
}

// Send issuance instructions to BNB via bridge message.
export async function sendIssuanceInstructions(
  runtime: OutbeRuntime,
  args: SendIssuanceInstructionsArgs,
): Promise<void> {
  const { telosis, publicClient, wallet } = runtime;

  const extraOptions = args.extraOptions ?? EMPTY_EXTRA_OPTIONS;
  let msgValue = toOptionalBigInt(args.msgValue);
  if (msgValue == null || msgValue === 0n) {
    const bridgeAddr = (await telosis.read.bridgeAdapter()) as Address;
    const bridge = (await runtime.viem.getContractAt("OriginMessenger", bridgeAddr)) as {
      read: { quoteSendIssuanceInstructions: (args: unknown[]) => Promise<{ nativeFee: bigint }> };
    };
    const config = await telosis.read.getAuctionConfig([args.seriesId]);
    const result = await telosis.read.getClearingResult([args.seriesId]);
    const quoted = await quoteAndBuffer(runtime, () =>
      bridge.read.quoteSendIssuanceInstructions([
        {
          seriesId: args.seriesId,
          promisLoadMinor: config.promisLoadMinor,
          costAmountMinor: config.costAmountMinor,
          floorPriceMinor: config.floorPriceMinor,
          intexCallPeriod: config.intexCallPeriod,
          settlementTokenAlias: config.settlementTokenAlias,
          callWindowDays: config.callWindowDays,
          callThresholdDays: config.callThresholdDays,
          callPriceMinor: config.callPriceMinor,
          recipients: result.winners,
          quantities: result.winnerQuantities,
        },
        extraOptions,
        false,
      ]),
    );
    msgValue = quoted > 0n ? quoted : DEFAULT_MSG_VALUE;
    if (quoted > 0n) console.log("[send-issuance-instructions] quoted fee:", quoted.toString(), "wei");
  }

  console.log("[send-issuance-instructions]", {
    seriesId: args.seriesId,
    msgValue: msgValue.toString(),
  });

  const tx = await telosis.write.sendIssuanceInstructions(
    [args.seriesId, extraOptions],
    { account: wallet.account, value: msgValue },
  );
  await publicClient.waitForTransactionReceipt({ hash: tx });
  console.log("[send-issuance-instructions] done tx:", tx);
}

// Send refund instructions to BNB via bridge message.
// This completes the auction lifecycle and marks auction as Settled.
export async function sendRefundInstructions(
  runtime: OutbeRuntime,
  args: SendRefundInstructionsArgs,
): Promise<void> {
  const { telosis, publicClient, wallet } = runtime;

  const extraOptions = args.extraOptions ?? EMPTY_EXTRA_OPTIONS;
  let msgValue = toOptionalBigInt(args.msgValue);
  if (msgValue == null || msgValue === 0n) {
    const bridgeAddr = (await telosis.read.bridgeAdapter()) as Address;
    const bridge = (await runtime.viem.getContractAt("OriginMessenger", bridgeAddr)) as {
      read: { quoteSendRefundInstructions: (args: unknown[]) => Promise<{ nativeFee: bigint }> };
    };
    const result = await telosis.read.getClearingResult([args.seriesId]);
    const quoted = await quoteAndBuffer(runtime, () =>
      bridge.read.quoteSendRefundInstructions([
        args.seriesId,
        result.allBidders,
        result.refundedAmounts,
        result.paidAmounts,
        extraOptions,
        false,
      ]),
    );
    msgValue = quoted > 0n ? quoted : DEFAULT_MSG_VALUE;
    if (quoted > 0n) console.log("[send-refund-instructions] quoted fee:", quoted.toString(), "wei");
  }

  console.log("[send-refund-instructions]", {
    seriesId: args.seriesId,
    msgValue: msgValue.toString(),
  });

  const tx = await telosis.write.sendRefundInstructions(
    [args.seriesId, extraOptions],
    { account: wallet.account, value: msgValue },
  );
  await publicClient.waitForTransactionReceipt({ hash: tx });
  console.log("[send-refund-instructions] done tx:", tx);
}

// =============================================================================
// TargetMessenger Pre-Funding
// =============================================================================

const FUND_THRESHOLD = parseEther("0.005");
const FUND_AMOUNT = parseEther("0.02");

const BSC_NETWORK_CONFIG: Record<string, { chain: typeof bscTestnet | typeof bsc; rpcEnv: string; pkEnv: string; defaultRpc: string }> = {
  bscTestnet: { chain: bscTestnet, rpcEnv: "BSC_TESTNET_RPC_URL", pkEnv: "BSC_TESTNET_PRIVATE_KEY", defaultRpc: "https://bsc-testnet.publicnode.com" },
  bsc: { chain: bsc, rpcEnv: "BSC_MAINNET_RPC_URL", pkEnv: "BSC_MAINNET_PRIVATE_KEY", defaultRpc: "https://bsc-dataseed1.binance.org" },
};

export interface FundBnbAdapterArgs {
  adapterAddress: Address;
  networkId?: string;
}

/**
 * Ensure TargetMessenger has enough native balance to pay LZ fees
 * for sending bids back to Outbe inside _handleAuctionStageClearing.
 */
export async function fundBnbBridgeAdapter(args: FundBnbAdapterArgs): Promise<void> {
  const netId = args.networkId ?? "bscTestnet";
  const cfg = BSC_NETWORK_CONFIG[netId];
  if (!cfg) throw new Error(`Unknown BSC network: ${netId}`);

  const rpc = process.env[cfg.rpcEnv] ?? cfg.defaultRpc;
  const pk = process.env[cfg.pkEnv];
  if (!pk) throw new Error(`${cfg.pkEnv} required to fund TargetMessenger on ${netId}`);

  const account = privateKeyToAccount(pk as `0x${string}`);
  const transport = http(rpc);
  const publicClient = createPublicClient({ chain: cfg.chain, transport });
  const walletClient = createWalletClient({ account, chain: cfg.chain, transport });

  const balance = await publicClient.getBalance({ address: args.adapterAddress });
  console.log(`[fund-adapter] TargetMessenger (${args.adapterAddress}) balance: ${formatEther(balance)} ${cfg.chain.nativeCurrency.symbol}`);

  if (balance >= FUND_THRESHOLD) {
    console.log(`[fund-adapter] Balance sufficient (>= ${formatEther(FUND_THRESHOLD)}), skipping.`);
    return;
  }

  console.log(`[fund-adapter] Sending ${formatEther(FUND_AMOUNT)} to adapter...`);
  const tx = await walletClient.sendTransaction({
    to: args.adapterAddress,
    value: FUND_AMOUNT,
    account,
  });
  await publicClient.waitForTransactionReceipt({ hash: tx });
  const newBalance = await publicClient.getBalance({ address: args.adapterAddress });
  console.log(`[fund-adapter] Funded. New balance: ${formatEther(newBalance)} (tx: ${tx})`);
}

// =============================================================================
// View Helpers
// =============================================================================

// Get auction stage name
export function getAuctionStageName(stage: number): string {
  const stages = ["None", "Started", "BidsReceived", "Cleared", "Settled"];
  return stages[stage] ?? `Unknown(${stage})`;
}

// Get series state name
export function getSeriesStateName(state: number): string {
  const states = ["Issued", "Qualified", "Called"];
  return states[state] ?? `Unknown(${state})`;
}

// Get auction info for display.
export async function getAuctionInfo(
  runtime: OutbeRuntime,
  seriesId: number,
): Promise<{
  stage: number;
  stageName: string;
  bidsCount: bigint;
  config: AuctionConfig;
}> {
  const { telosis } = runtime;

  const stage = await telosis.read.getAuctionStage([seriesId]);
  const config = await telosis.read.getAuctionConfig([seriesId]);
  const bidsCount = await telosis.read.getBidsCount([seriesId]);

  return {
    stage,
    stageName: getAuctionStageName(stage),
    bidsCount,
    config,
  };
}

// Get bids for auction.
export async function getAuctionBids(
  runtime: OutbeRuntime,
  seriesId: number,
): Promise<readonly BidData[]> {
  return await runtime.telosis.read.getBids([seriesId]);
}

// =============================================================================
// LZ Delivery Waiting (Outbe → BSC)
// =============================================================================

const OAPP_PEER_ABI = [
  { inputs: [{ name: "eid", type: "uint32" }], name: "peers", outputs: [{ type: "bytes32" }], stateMutability: "view", type: "function" },
  { inputs: [], name: "BNB_EID", outputs: [{ type: "uint32" }], stateMutability: "view", type: "function" },
] as const;

export interface WaitForLzDeliveryArgs {
  bridgeAdapterAddress: Address;
  bscNetworkId?: string;
  outbeNetworkId?: string;
  pollIntervalMs?: number;
  maxPolls?: number;
}

/**
 * Wait until the latest outbound nonce from OriginMessenger is delivered on BSC.
 * Reads outboundNonce on Outbe, then polls lazyInboundNonce on BSC until it catches up.
 */
export async function waitForLzDelivery(
  runtime: OutbeRuntime,
  args: WaitForLzDeliveryArgs,
): Promise<{ outbound: bigint; delivered: bigint }> {
  const pollMs = args.pollIntervalMs ?? 5_000;
  const maxPolls = args.maxPolls ?? 120;
  const netId = args.bscNetworkId ?? "bscTestnet";
  const outbeNetId = args.outbeNetworkId ?? "outbeDevnet";
  const srcEid = NETWORK_TO_EID[outbeNetId];
  if (srcEid == null) throw new Error(`Unknown Outbe network: ${outbeNetId}`);
  const cfg = BSC_NETWORK_CONFIG[netId];
  if (!cfg) throw new Error(`Unknown BSC network: ${netId}`);

  const outbePublic = await (runtime.viem as unknown as { getPublicClient(): Promise<ViemPublicClient> }).getPublicClient();

  // Read BNB_EID and peer from OriginMessenger
  const bnbEid = await outbePublic.readContract({
    address: args.bridgeAdapterAddress,
    abi: OAPP_PEER_ABI,
    functionName: "BNB_EID",
  });
  const peerBytes32 = await outbePublic.readContract({
    address: args.bridgeAdapterAddress,
    abi: OAPP_PEER_ABI,
    functionName: "peers",
    args: [bnbEid],
  });
  const peerAddress = ("0x" + peerBytes32.slice(-40)) as Address;

  // Read current outbound nonce on Outbe
  const outbound = await outbePublic.readContract({
    address: LZ_INFRA.endpoint,
    abi: ENDPOINT_NONCE_ABI,
    functionName: "outboundNonce",
    args: [args.bridgeAdapterAddress, bnbEid, peerBytes32],
  });

  // Create BSC public client for polling
  const bscRpc = process.env[cfg.rpcEnv] ?? cfg.defaultRpc;
  const bscPublic = createPublicClient({ chain: cfg.chain, transport: http(bscRpc) });
  const senderBytes32 = addressToBytes32(args.bridgeAdapterAddress as `0x${string}`);

  console.log(`[lz-wait] Waiting for nonce ${outbound} to be delivered on BSC...`);
  console.log(`[lz-wait]   OriginMessenger: ${args.bridgeAdapterAddress}`);
  console.log(`[lz-wait]   TargetMessenger:   ${peerAddress}`);

  for (let i = 0; i < maxPolls; i++) {
    const delivered = await bscPublic.readContract({
      address: LZ_INFRA.endpoint,
      abi: ENDPOINT_NONCE_ABI,
      functionName: "lazyInboundNonce",
      args: [peerAddress, srcEid, senderBytes32],
    });

    if (delivered >= outbound) {
      console.log(`[lz-wait] Delivered! lazyInboundNonce=${delivered}, outboundNonce=${outbound}`);
      return { outbound, delivered };
    }

    if (i === 0 || i % 6 === 0) {
      console.log(`[lz-wait]   lazyInboundNonce=${delivered}, waiting for ${outbound}...`);
    }
    await new Promise((r) => setTimeout(r, pollMs));
  }

  const finalDelivered = await bscPublic.readContract({
    address: LZ_INFRA.endpoint,
    abi: ENDPOINT_NONCE_ABI,
    functionName: "lazyInboundNonce",
    args: [peerAddress, srcEid, senderBytes32],
  });
  const timeoutMsg = `[lz-wait] Timeout! lazyInboundNonce=${finalDelivered}, expected=${outbound}`;
  console.error(timeoutMsg);
  throw new Error(timeoutMsg);
}
