//Auction Flow Script
/**
 * Manages the complete auction lifecycle:
 * - Start auction
 * - Start reveal stage
 * - Start clearing stage
 * - Execute clearing
 * - Finalize escrow
 */

import { normalizeSeries, seriesIdToNoonTimestamp, seriesIdToUint32 } from "../shared/auctionId.js";
import type { Address, Hex } from "viem";

// =============================================================================
// Types
// =============================================================================

/** Wallet account for contract interactions */
export interface WalletAccount {
  address: `0x${string}`;
}

/** Wallet client with account */
export interface WalletClient {
  account: WalletAccount;
}

/** Public client for transaction operations */
export interface PublicClient {
  waitForTransactionReceipt(args: { hash: `0x${string}` }): Promise<void>;
}

/** Auction schedule — stage-end timestamps, mirrors the on-chain AuctionSchedule struct */
export interface AuctionSchedule {
  commitEnd: number;
  revealEnd: number;
  issuanceEnd: number;
}

/** Auction input parameters, mirrors the on-chain AuctionParams struct */
export interface AuctionParams {
  intexSize: bigint;
  minIntexBidPrice: bigint;
  intexStrikePrice: bigint;
  coenPriceFloor: bigint;
  minIntexBidQuantity: number;
}

/** Auction contract read API */
export interface AuctionReadApi {
  getAuctionDetails(args: [number]): Promise<readonly [AuctionData, SubmittedBidData[]]>;
  getAuctionStage(args: [number]): Promise<number>;
  escrowContract(): Promise<Address>;
}

/** Auction contract write API */
export interface AuctionWriteApi {
  auctionStart(
    args: [number, AuctionSchedule, AuctionParams],
    opts: { account: WalletAccount },
  ): Promise<`0x${string}`>;
  startRevealingBidsStage(
    args: [number, boolean],
    opts: { account: WalletAccount },
  ): Promise<`0x${string}`>;
  startClearingStage(
    args: [number],
    opts: { account: WalletAccount },
  ): Promise<`0x${string}`>;
  executeAuctionClearing(
    args: [number, number, bigint, number],
    opts: { account: WalletAccount },
  ): Promise<`0x${string}`>;
}

/** Auction contract interface */
export interface AuctionContract {
  read: AuctionReadApi;
  write: AuctionWriteApi;
}

/** EscrowAdapter read API */
export interface EscrowAdapterReadApi {
  getBidLock(args: [number, Address]): Promise<{ lockedAmount: bigint; status: number; lockedAt: number }>;
  getAuctionStatus(args: [number]): Promise<readonly [boolean, boolean, bigint]>;
}

/** EscrowAdapter write API */
export interface EscrowAdapterWriteApi {
  finalizeAuction(
    args: [number, FinalizationInstruction[]],
    opts: { account: WalletAccount },
  ): Promise<`0x${string}`>;
}

/** EscrowAdapter contract interface */
export interface EscrowAdapterContract {
  read: EscrowAdapterReadApi;
  write: EscrowAdapterWriteApi;
  _contract?: unknown;
}

/** Viem client interface */
export interface ViemClient {
  getContractAt(abi: string | readonly unknown[], address: Address): Promise<unknown>;
  getPublicClient(): Promise<unknown>;
  getWalletClients(): Promise<readonly unknown[]>;
}

/** Runtime context for auction operations */
export interface AuctionRuntime {
  address: `0x${string}`;
  contract: AuctionContract;
  escrowAdapter?: EscrowAdapterContract;
  escrowAdapterAddress?: Address;
  paymentTokenAddress?: Address;
  viem?: ViemClient;
  publicClient: PublicClient;
  wallet: WalletClient;
}

/** Auction data from contract — mirrors the on-chain AuctionData struct */
export interface AuctionData {
  worldwideDayState: number;
  schedule: AuctionSchedule;
  params: AuctionParams;
  result: {
    issuedIntexLoadedPromis: bigint;
    auctionIntexClearingPrice: bigint;
    issuedIntexCount: number;
    wonBidsCount: number;
  };
}

/** Submitted bid data from contract — mirrors the on-chain SubmittedBidData struct */
export interface SubmittedBidData {
  bidderAddress: `0x${string}`;
  intexBidPrice: bigint;
  timestamp: number;
  intexQuantity: number;
}

/** Instruction for finalizing a bidder's escrow — mirrors the on-chain FinalizationInstruction struct */
export interface FinalizationInstruction {
  bidder: Address;
  refundedAmount: bigint;
  paidAmount: bigint;
}

// =============================================================================
// Argument Types
// =============================================================================

type BigIntInput = string | number | bigint | undefined;

type CommonArgs = {
  address?: string;
  series?: string;
};

export type AuctionFlowArgs = CommonArgs & {
  floor?: BigIntInput;
  commitEnd?: BigIntInput;
  revealEnd?: BigIntInput;
  issuanceEnd?: BigIntInput;
  intexSize?: BigIntInput;
  intexStrikePrice?: BigIntInput;
  coenPriceFloor?: BigIntInput;
  minIntexBidQuantity?: BigIntInput;
  isGreenDay?: string | boolean;
  skipStart?: boolean;
  skipReveal?: boolean;
  skipClearing?: boolean;
  skipExecute?: boolean;
  issuedIntexCount?: BigIntInput;
  clearingPrice?: BigIntInput;
  wonBidsCount?: BigIntInput;
  finalizeEscrow?: boolean;
  escrowAdapterAddress?: string;
  paymentTokenAddress?: string;
};

export type AuctionStartArgs = CommonArgs & {
  floor?: BigIntInput;
  commitEnd?: BigIntInput;
  revealEnd?: BigIntInput;
  issuanceEnd?: BigIntInput;
  intexSize?: BigIntInput;
  intexStrikePrice?: BigIntInput;
  coenPriceFloor?: BigIntInput;
  minIntexBidQuantity?: BigIntInput;
};

export type AuctionRevealArgs = CommonArgs & {
  isGreenDay?: string | boolean;
};

export type AuctionClearingArgs = CommonArgs;

export type AuctionExecuteArgs = CommonArgs & {
  issuedIntexCount?: BigIntInput;
  clearingPrice?: BigIntInput;
  wonBidsCount?: BigIntInput;
  finalizeEscrow?: boolean;
  escrowAdapterAddress?: string;
  paymentTokenAddress?: string;
};

// =============================================================================
// Constants
// =============================================================================

const DEFAULT_FLOOR = 1080n;
const DEFAULT_INTEX_SIZE = 1_000_000n;
const DEFAULT_INTEX_STRIKE_PRICE = 80_000_000n;
const DEFAULT_COEN_PRICE_FLOOR = 1080n;
const DEFAULT_MIN_INTEX_BID_QUANTITY = 1;

// Schedule offsets (seconds) used when explicit stage-end timestamps are not provided.
const COMMIT_WINDOW_SECONDS = 3600;
const REVEAL_WINDOW_SECONDS = 3600;
const ISSUANCE_WINDOW_SECONDS = 3600;

// =============================================================================
// Utilities
// =============================================================================

/** Convert input to bigint, return undefined for empty/undefined */
function toOptionalBigInt(input: BigIntInput): bigint | undefined {
  if (input === undefined) return undefined;
  if (typeof input === "bigint") return input;
  if (typeof input === "number") return BigInt(input);
  const trimmed = String(input).trim();
  return trimmed === "" ? undefined : BigInt(trimmed);
}

/** Convert input to number, return undefined for empty/undefined */
function toOptionalNumber(input: BigIntInput): number | undefined {
  const b = toOptionalBigInt(input);
  return b === undefined ? undefined : Number(b);
}

/** Parse floor price with default */
function parseFloor(floor?: BigIntInput): bigint {
  return toOptionalBigInt(floor) ?? DEFAULT_FLOOR;
}

/** Parse boolean from string/boolean with default */
function parseBoolean(value?: string | boolean, defaultValue = true): boolean {
  if (typeof value === "boolean") return value;
  if (value === undefined || value === "") return defaultValue;
  return ["true", "1", "yes"].includes(value.toLowerCase());
}

/** Build an auction schedule from explicit args or derive it from the series noon timestamp. */
function resolveSchedule(
  seriesId: string,
  args: { commitEnd?: BigIntInput; revealEnd?: BigIntInput; issuanceEnd?: BigIntInput },
): AuctionSchedule {
  const anchor = Number(seriesIdToNoonTimestamp(seriesId));
  const commitEnd = toOptionalNumber(args.commitEnd) ?? anchor + COMMIT_WINDOW_SECONDS;
  const revealEnd = toOptionalNumber(args.revealEnd) ?? commitEnd + REVEAL_WINDOW_SECONDS;
  const issuanceEnd = toOptionalNumber(args.issuanceEnd) ?? revealEnd + ISSUANCE_WINDOW_SECONDS;
  return { commitEnd, revealEnd, issuanceEnd };
}

// =============================================================================
// Auction Stage Operations
// =============================================================================

/** Start a new auction */
async function startAuction(
  runtime: AuctionRuntime,
  opts: {
    seriesId: string;
    schedule: AuctionSchedule;
    params: AuctionParams;
  },
): Promise<void> {
  const { contract, wallet, publicClient } = runtime;
  const seriesIdNum = seriesIdToUint32(opts.seriesId);

  console.log("[auction-start]", {
    address: runtime.address,
    seriesId: seriesIdNum,
    schedule: opts.schedule,
    params: {
      intexSize: opts.params.intexSize.toString(),
      minIntexBidPrice: opts.params.minIntexBidPrice.toString(),
      intexStrikePrice: opts.params.intexStrikePrice.toString(),
      coenPriceFloor: opts.params.coenPriceFloor.toString(),
      minIntexBidQuantity: opts.params.minIntexBidQuantity,
    },
  });

  const tx = await contract.write.auctionStart(
    [seriesIdNum, opts.schedule, opts.params],
    { account: wallet.account },
  );
  await publicClient.waitForTransactionReceipt({ hash: tx });
  console.log("[auction-start] done tx:", tx);
}

/** Start revealing bids stage */
async function startRevealing(
  runtime: AuctionRuntime,
  opts: { seriesId: number; isGreenDay: boolean },
): Promise<void> {
  const { contract, wallet, publicClient } = runtime;

  const tx = await contract.write.startRevealingBidsStage([opts.seriesId, opts.isGreenDay], {
    account: wallet.account,
  });
  await publicClient.waitForTransactionReceipt({ hash: tx });
  console.log("[auction-reveal] done tx:", tx);
}

/** Start clearing stage */
async function startClearing(runtime: AuctionRuntime, opts: { seriesId: number }): Promise<void> {
  const { contract, wallet, publicClient } = runtime;

  const tx = await contract.write.startClearingStage([opts.seriesId], {
    account: wallet.account,
  });
  await publicClient.waitForTransactionReceipt({ hash: tx });
  console.log("[auction-clearing] done tx:", tx);
}

/** Execute auction clearing */
async function executeClearing(
  runtime: AuctionRuntime,
  opts: {
    seriesId: number;
    minimumBidPrice: bigint;
    issuedOverride?: number;
    clearingOverride?: bigint;
    wonBidsOverride?: number;
  },
): Promise<bigint> {
  const { contract, wallet, publicClient } = runtime;
  const [, bids] = await contract.read.getAuctionDetails([opts.seriesId]);

  // Issued Intex count (sum of all bid quantities)
  const issuedIntexCount =
    opts.issuedOverride ?? bids.reduce((sum, b) => sum + Number(b.intexQuantity), 0);

  // Clearing price = minimum revealed bid price (or floor price if no bids)
  const clearingPrice =
    opts.clearingOverride ??
    (bids.length === 0
      ? opts.minimumBidPrice
      : bids.reduce(
          (min, b) => (b.intexBidPrice < min ? b.intexBidPrice : min),
          bids[0]?.intexBidPrice ?? opts.minimumBidPrice,
        ));

  // Number of winning bids — defaults to every revealed bid.
  const wonBidsCount = opts.wonBidsOverride ?? bids.length;

  console.log("[auction-execute]", {
    seriesId: opts.seriesId,
    issuedIntexCount,
    clearingPrice: clearingPrice.toString(),
    wonBidsCount,
  });

  const tx = await contract.write.executeAuctionClearing(
    [opts.seriesId, issuedIntexCount, clearingPrice, wonBidsCount],
    { account: wallet.account },
  );
  await publicClient.waitForTransactionReceipt({ hash: tx });
  console.log("[auction-execute] done tx:", tx);

  return clearingPrice;
}

// =============================================================================
// Escrow Operations
// =============================================================================

/** Finalize escrow for all bidders */
async function finalizeEscrow(
  runtime: AuctionRuntime,
  opts: {
    seriesId: number;
    clearingPrice: bigint;
    escrowAdapterAddress?: string;
    paymentTokenAddress?: string;
  },
): Promise<void> {
  // Initialize EscrowAdapter if needed
  if (!runtime.escrowAdapter || !runtime.escrowAdapterAddress) {
    if (!runtime.viem) {
      throw new Error("Viem client not available. Use createAuctionRuntime from shared/runtime.ts");
    }

    const escrowAddress: Address = opts.escrowAdapterAddress
      ? (opts.escrowAdapterAddress as Address)
      : await runtime.contract.read.escrowContract();

    const escrowContract = (await runtime.viem.getContractAt("EscrowAdapter", escrowAddress)) as {
      read: EscrowAdapterReadApi;
      write: EscrowAdapterWriteApi;
    };

    runtime.escrowAdapter = {
      read: {
        getBidLock: escrowContract.read.getBidLock.bind(escrowContract.read),
        getAuctionStatus: escrowContract.read.getAuctionStatus.bind(escrowContract.read),
      },
      write: {
        finalizeAuction: escrowContract.write.finalizeAuction.bind(escrowContract.write),
      },
      _contract: escrowContract,
    };
    runtime.escrowAdapterAddress = escrowAddress;
  }

  const { contract, wallet, publicClient, viem } = runtime;
  const [, bids] = await contract.read.getAuctionDetails([opts.seriesId]);

  if (bids.length === 0) {
    console.log("[finalize-escrow] No bids to finalize");
    return;
  }

  console.log("[finalize-escrow]", {
    seriesId: opts.seriesId,
    clearingPrice: opts.clearingPrice.toString(),
    bidsCount: bids.length,
    escrowAdapterAddress: runtime.escrowAdapterAddress,
  });

  // Get escrow contract for direct calls
  const escrowContract = (await viem!.getContractAt("EscrowAdapter", runtime.escrowAdapterAddress!)) as {
    read: EscrowAdapterReadApi;
    write: EscrowAdapterWriteApi;
  };

  const instructions: FinalizationInstruction[] = [];

  for (const bid of bids) {
    const bidderAddress = bid.bidderAddress as Address;
    const quantity = BigInt(bid.intexQuantity);

    // Get bid lock status
    const bidLock = await escrowContract.read.getBidLock([opts.seriesId, bidderAddress]);

    // Only process bids with active locks (status === 1 = Locked)
    if (bidLock.status !== 1) {
      console.log(
        `[finalize-escrow] Skipping ${bidderAddress}: status=${bidLock.status} (expected 1=Locked)`,
      );
      continue;
    }

    const lockedAmount = bidLock.lockedAmount;
    if (lockedAmount === 0n) {
      console.log(`[finalize-escrow] Skipping ${bidderAddress}: locked amount is 0`);
      continue;
    }

    // Calculate amounts
    const usedAmount = quantity * opts.clearingPrice;
    let refundedAmount: bigint;
    let paidAmount: bigint;

    if (usedAmount >= lockedAmount) {
      paidAmount = lockedAmount;
      refundedAmount = 0n;
    } else {
      paidAmount = usedAmount;
      refundedAmount = lockedAmount - usedAmount;
    }

    instructions.push({ bidder: bidderAddress, refundedAmount, paidAmount });

    console.log(`[finalize-escrow] ${bidderAddress}:`, {
      locked: lockedAmount.toString(),
      used: usedAmount.toString(),
      refunded: refundedAmount.toString(),
      paid: paidAmount.toString(),
    });
  }

  if (instructions.length === 0) {
    console.log("[finalize-escrow] No active locks to finalize");
    return;
  }

  console.log("[finalize-escrow] Calling finalizeAuction with", instructions.length, "instructions");
  const tx = await escrowContract.write.finalizeAuction([opts.seriesId, instructions], {
    account: wallet.account,
  });
  await publicClient.waitForTransactionReceipt({ hash: tx });
  console.log("[finalize-escrow] done tx:", tx);
}

// =============================================================================
// Token Approval (for external use)
// =============================================================================

/** Approve payment tokens for EscrowAdapter */
export async function approveEscrowTokens(
  runtime: AuctionRuntime,
  opts: {
    amount: bigint;
    paymentTokenAddress: Address;
    escrowAdapterAddress: Address;
  },
): Promise<Hex> {
  const { wallet, publicClient, viem } = runtime;

  if (!viem) {
    throw new Error("Viem client not available. Use createAuctionRuntime from shared/runtime.ts");
  }

  const paymentToken = (await viem.getContractAt(
    [
      "function approve(address spender, uint256 amount) returns (bool)",
      "function allowance(address owner, address spender) view returns (uint256)",
    ],
    opts.paymentTokenAddress,
  )) as {
    read: { allowance(args: [Address, Address]): Promise<bigint> };
    write: { approve(args: [Address, bigint], opts: { account: WalletAccount }): Promise<Hex> };
  };

  console.log("[approve-escrow]", {
    paymentToken: opts.paymentTokenAddress,
    escrowAdapter: opts.escrowAdapterAddress,
    amount: opts.amount.toString(),
    bidder: wallet.account.address,
  });

  const currentAllowance = await paymentToken.read.allowance([
    wallet.account.address,
    opts.escrowAdapterAddress,
  ]);

  if (currentAllowance >= opts.amount) {
    console.log("[approve-escrow] Already approved, skipping");
    return `0x${"0".repeat(64)}` as Hex;
  }

  const tx = await paymentToken.write.approve([opts.escrowAdapterAddress, opts.amount], {
    account: wallet.account,
  });
  await publicClient.waitForTransactionReceipt({ hash: tx });
  console.log("[approve-escrow] done tx:", tx);

  return tx;
}

// =============================================================================
// Parameter Resolution
// =============================================================================

/** Build the AuctionParams struct from CLI args, applying defaults. */
function resolveParams(args: {
  floor?: BigIntInput;
  intexSize?: BigIntInput;
  intexStrikePrice?: BigIntInput;
  coenPriceFloor?: BigIntInput;
  minIntexBidQuantity?: BigIntInput;
}): AuctionParams {
  return {
    intexSize: toOptionalBigInt(args.intexSize) ?? DEFAULT_INTEX_SIZE,
    minIntexBidPrice: parseFloor(args.floor),
    intexStrikePrice: toOptionalBigInt(args.intexStrikePrice) ?? DEFAULT_INTEX_STRIKE_PRICE,
    coenPriceFloor: toOptionalBigInt(args.coenPriceFloor) ?? DEFAULT_COEN_PRICE_FLOOR,
    minIntexBidQuantity:
      toOptionalNumber(args.minIntexBidQuantity) ?? DEFAULT_MIN_INTEX_BID_QUANTITY,
  };
}

// =============================================================================
// Main Flow Operations
// =============================================================================

/** Run complete auction flow */
export async function runAuctionFlowCore(opts: AuctionFlowArgs & { runtime: AuctionRuntime }) {
  const { runtime } = opts;
  const seriesId = normalizeSeries(opts.series);
  const seriesIdNum = seriesIdToUint32(seriesId);
  const schedule = resolveSchedule(seriesId, opts);
  const params = resolveParams(opts);
  const isGreen = parseBoolean(opts.isGreenDay, true);
  const issuedOverride = toOptionalNumber(opts.issuedIntexCount);
  const clearingOverride = toOptionalBigInt(opts.clearingPrice);
  const wonBidsOverride = toOptionalNumber(opts.wonBidsCount);

  if (!opts.skipStart) {
    await startAuction(runtime, { seriesId, schedule, params });
  }

  if (!opts.skipReveal) {
    await startRevealing(runtime, { seriesId: seriesIdNum, isGreenDay: isGreen });
  }

  if (!opts.skipClearing) {
    await startClearing(runtime, { seriesId: seriesIdNum });
  }

  if (!opts.skipExecute) {
    const clearingPrice = await executeClearing(runtime, {
      seriesId: seriesIdNum,
      minimumBidPrice: params.minIntexBidPrice,
      issuedOverride,
      clearingOverride,
      wonBidsOverride,
    });

    if (opts.finalizeEscrow !== false) {
      await finalizeEscrow(runtime, {
        seriesId: seriesIdNum,
        clearingPrice,
        escrowAdapterAddress: opts.escrowAdapterAddress,
        paymentTokenAddress: opts.paymentTokenAddress,
      });
    }
  }

  console.log("[done] Auction flow completed for", seriesIdNum);
}

/** Start auction only */
export async function runAuctionStartCore(opts: AuctionStartArgs & { runtime: AuctionRuntime }) {
  const seriesId = normalizeSeries(opts.series);
  const schedule = resolveSchedule(seriesId, opts);
  const params = resolveParams(opts);

  await startAuction(opts.runtime, { seriesId, schedule, params });
}

/** Start reveal stage only */
export async function runAuctionRevealCore(opts: AuctionRevealArgs & { runtime: AuctionRuntime }) {
  const seriesId = seriesIdToUint32(normalizeSeries(opts.series));
  const isGreen = parseBoolean(opts.isGreenDay, true);
  await startRevealing(opts.runtime, { seriesId, isGreenDay: isGreen });
}

/** Start clearing stage only */
export async function runAuctionClearingCore(opts: AuctionClearingArgs & { runtime: AuctionRuntime }) {
  const seriesId = seriesIdToUint32(normalizeSeries(opts.series));
  await startClearing(opts.runtime, { seriesId });
}

/** Execute clearing only */
export async function runAuctionExecuteCore(opts: AuctionExecuteArgs & { runtime: AuctionRuntime }) {
  const seriesId = seriesIdToUint32(normalizeSeries(opts.series));

  const clearingPrice = await executeClearing(opts.runtime, {
    seriesId,
    minimumBidPrice: DEFAULT_FLOOR,
    issuedOverride: toOptionalNumber(opts.issuedIntexCount),
    clearingOverride: toOptionalBigInt(opts.clearingPrice),
    wonBidsOverride: toOptionalNumber(opts.wonBidsCount),
  });

  if (opts.finalizeEscrow !== false) {
    await finalizeEscrow(opts.runtime, {
      seriesId,
      clearingPrice,
      escrowAdapterAddress: opts.escrowAdapterAddress,
      paymentTokenAddress: opts.paymentTokenAddress,
    });
  }
}
