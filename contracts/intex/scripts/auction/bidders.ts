// Auction Bidders Script
// Handles commit and reveal operations for multiple bidders.
// Uses pure viem clients for ERC20 approve operations to avoid Hardhat plugin issues.

import {
  keccak256,
  createPublicClient,
  createWalletClient,
  http,
  parseAbi,
  type Address,
  type Hex,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { bscTestnet } from "viem/chains";

// =============================================================================
// Types
// =============================================================================

/** Wallet entry from JSON file */
export interface WalletEntry {
  address: string;
  privateKey: string;
  startBalance?: {
    tBNB?: string;
    USDC?: string;
  };
}

/** Bid parameters */
export interface BidParams {
  quantity: number;
  bidPrice: string;
}

/** Commit record persisted to file */
export interface CommitRecord {
  bidder: string;
  seriesId: number;
  commitHash: Hex;
  bidParams: BidParams;
  txHash?: Hex;
  revealed?: boolean;
}

/** Viem client interface for Hardhat runtime */
export interface ViemClient {
  getContractAt(abi: string | readonly unknown[], address: Address): Promise<unknown>;
  getPublicClient(): Promise<unknown>;
  getWalletClients(): Promise<readonly unknown[]>;
}

/** Auction params mirroring the on-chain AuctionParams struct */
export interface AuctionParams {
  promisLoadMinor: bigint;
  minIntexBidPrice: bigint;
  costAmountMinor: bigint;
  floorPriceMinor: bigint;
  minIntexBidQuantity: number;
}

/** Auction info from contract — mirrors the on-chain AuctionData struct */
export interface AuctionInfo {
  worldwideDayState: number;
  schedule: {
    commitEnd: number;
    revealEnd: number;
    issuanceEnd: number;
  };
  params: AuctionParams;
  result: {
    issuedIntexLoadedPromis: bigint;
    auctionIntexClearingPrice: bigint;
    issuedIntexCount: number;
    wonBidsCount: number;
  };
}

/** Wallet account for contract interactions */
export interface WalletAccount {
  address: `0x${string}`;
}

/** Transaction receipt */
export interface TransactionReceipt {
  status: "success" | "reverted";
  transactionHash: Hex;
  blockNumber: bigint;
}

/** Public client interface */
export interface PublicClient {
  waitForTransactionReceipt(args: { hash: Hex }): Promise<TransactionReceipt>;
  getChainId(): Promise<number>;
}

/** Auction contract interface for bidder operations */
export interface AuctionBidderContract {
  read: {
    getAuctionInfo(args: [number]): Promise<AuctionInfo>;
    getAuctionStage(args: [number]): Promise<number>;
    escrowContract?(): Promise<Address>;
  };
  write: {
    commitBid(args: [number, Hex], opts: { account: WalletAccount }): Promise<Hex>;
    revealBid(
      args: [number, bigint, bigint, bigint, Hex],
      opts: { account: WalletAccount },
    ): Promise<Hex>;
  };
}

/** Runtime context for bidder operations */
export interface AuctionBidderRuntime {
  address: `0x${string}`;
  contract: AuctionBidderContract;
  publicClient: PublicClient;
  chainId: number;
  viem?: ViemClient;
}

/** Options for commit operation */
export interface CommitBidsOptions {
  seriesId: number;
  wallets: WalletEntry[];
  quantityRange?: [number, number];
  priceRange?: [string, string];
  commitsFile?: string;
}

/** Options for reveal operation */
export interface RevealBidsOptions {
  seriesId: number;
  wallets: WalletEntry[];
  commitsFile: string;
  escrowAdapterAddress?: Address;
  paymentTokenAddress?: Address;
}

// =============================================================================
// Constants
// =============================================================================

// Payment-token denomination (6 decimals).
const STABLE_DECIMALS = 6;
const STABLE_MULTIPLIER = 10 ** STABLE_DECIMALS;

/** ERC20 ABI for approve and allowance operations */
const ERC20_ABI = parseAbi([
  "function approve(address spender, uint256 amount) returns (bool)",
  "function allowance(address owner, address spender) view returns (uint256)",
]);

// =============================================================================
// Public API - Signature Generation
// =============================================================================

/**
 * EIP-712 typed data signature matching `IntexAuction._verifyRevealSignature`.
 * Domain: `IntexAuction` v1, bound to chainId and the auction `verifyingContract`.
 */
async function signRevealBid(
  seriesId: number,
  bidderAddress: Address,
  quantity: bigint,
  bidPrice: bigint,
  chainId: bigint,
  auctionAddress: Address,
  privateKey: Hex,
): Promise<Hex> {
  const account = privateKeyToAccount(privateKey);
  return account.signTypedData({
    domain: {
      name: "IntexAuction",
      version: "1",
      chainId: Number(chainId),
      verifyingContract: auctionAddress,
    },
    types: {
      RevealBid: [
        { name: "seriesId", type: "uint32" },
        { name: "bidder", type: "address" },
        { name: "quantity", type: "uint16" },
        { name: "bidPrice", type: "uint64" },
      ],
    },
    primaryType: "RevealBid",
    message: { seriesId, bidder: bidderAddress, quantity: Number(quantity), bidPrice },
  });
}

/** Create commit hash for commitBid(): keccak256(EIP-712 signature). */
export async function createCommitHash(
  seriesId: number,
  bidderAddress: Address,
  quantity: bigint,
  bidPrice: bigint,
  chainId: bigint,
  auctionAddress: Address,
  privateKey: Hex,
): Promise<Hex> {
  const sig = await signRevealBid(seriesId, bidderAddress, quantity, bidPrice, chainId, auctionAddress, privateKey);
  return keccak256(sig);
}

/** Create reveal signature for revealBid(): the raw 65-byte EIP-712 signature. */
export async function createRevealSignature(
  seriesId: number,
  bidderAddress: Address,
  quantity: bigint,
  bidPrice: bigint,
  chainId: bigint,
  auctionAddress: Address,
  privateKey: Hex,
): Promise<Hex> {
  return signRevealBid(seriesId, bidderAddress, quantity, bidPrice, chainId, auctionAddress, privateKey);
}

// =============================================================================
// File I/O
// =============================================================================

async function loadCommitsFile(filePath: string): Promise<CommitRecord[]> {
  try {
    const fs = await import("fs");
    if (!fs.existsSync(filePath)) return [];

    const raw = JSON.parse(fs.readFileSync(filePath, "utf8")) as CommitRecord[];
    return raw.map((commit) => ({
      ...commit,
      bidParams: {
        quantity: commit.bidParams.quantity,
        bidPrice: commit.bidParams.bidPrice,
      },
    }));
  } catch (error) {
    if (error instanceof Error && !error.message.includes("ENOENT")) {
      console.warn(`[commits] Failed to load ${filePath}:`, error.message);
    }
    return [];
  }
}

async function saveCommitsFile(filePath: string, commits: CommitRecord[]): Promise<void> {
  const fs = await import("fs");
  const serialized = JSON.stringify(
    commits,
    (_key, value) => (typeof value === "bigint" ? value.toString() : value),
    2,
  );
  fs.writeFileSync(filePath, serialized);
}

// =============================================================================
// Bid Generation
// =============================================================================

interface GeneratedBid {
  quantity: bigint;
  bidPrice: bigint;
  priceFloat: number;
}

function generateRandomBid(
  quantityRange: [number, number],
  priceRange: [string, string] | undefined,
  floorPrice: bigint,
): GeneratedBid {
  // Generate random quantity
  const quantity = BigInt(
    Math.floor(Math.random() * (quantityRange[1] - quantityRange[0] + 1)) + quantityRange[0],
  );

  // Determine price range
  let minPrice: number;
  let maxPrice: number;

  if (priceRange) {
    minPrice = parseFloat(priceRange[0]);
    maxPrice = parseFloat(priceRange[1]);
    if (isNaN(minPrice) || isNaN(maxPrice) || minPrice >= maxPrice) {
      throw new Error(`Invalid priceRange: [${priceRange[0]}, ${priceRange[1]}]`);
    }
  } else {
    // Default: floorPrice to floorPrice + 10 payment-token units
    minPrice = Number(floorPrice) / STABLE_MULTIPLIER;
    maxPrice = Number(floorPrice + 10_000_000n) / STABLE_MULTIPLIER;
  }

  // Generate random price
  const priceFloat = Math.random() * (maxPrice - minPrice) + minPrice;
  const bidPrice = BigInt(Math.floor(priceFloat * STABLE_MULTIPLIER));

  return { quantity, bidPrice, priceFloat };
}

// =============================================================================
// ERC20 Approve (Pure Viem)
// =============================================================================

/**
 * Approve EscrowAdapter to spend payment tokens before reveal.
 * Uses pure viem clients to avoid Hardhat plugin issues with human-readable ABIs.
 */
async function approvePaymentToken(
  bidderAddress: Address,
  bidderPrivateKey: Hex,
  escrowAdapterAddress: Address,
  paymentTokenAddress: Address,
  amount: bigint,
  chainId: number,
): Promise<boolean> {
  if (!escrowAdapterAddress || !paymentTokenAddress) {
    console.warn(`[approve] Missing addresses, skipping for ${bidderAddress}`);
    return false;
  }

  try {
    // Create viem clients directly (not through Hardhat)
    const chain = chainId === 97 ? bscTestnet : bscTestnet; // Extend for other chains
    const transport = http();

    const publicClient = createPublicClient({ chain, transport });
    const account = privateKeyToAccount(bidderPrivateKey);
    const walletClient = createWalletClient({ account, chain, transport });

    // Check current allowance
    const currentAllowance = await publicClient.readContract({
      address: paymentTokenAddress,
      abi: ERC20_ABI,
      functionName: "allowance",
      args: [bidderAddress, escrowAdapterAddress],
    });

    if (currentAllowance >= amount) {
      console.log(`[approve] ${bidderAddress} has sufficient allowance (${currentAllowance})`);
      return true;
    }

    console.log(`[approve] ${bidderAddress} approving ${amount} to ${escrowAdapterAddress}`);

    const hash = await walletClient.writeContract({
      address: paymentTokenAddress,
      abi: ERC20_ABI,
      functionName: "approve",
      args: [escrowAdapterAddress, amount],
    });

    await publicClient.waitForTransactionReceipt({ hash });
    console.log(`[approve] ${bidderAddress} approved, tx: ${hash}`);
    return true;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(`[approve] Error for ${bidderAddress}:`, message);
    return false;
  }
}

// =============================================================================
// Main Operations
// =============================================================================

const LZ_DELIVERY_POLL_MS = 5_000;
const LZ_DELIVERY_MAX_ATTEMPTS = 24; // ~2 min

/** Poll until auction exists on BNB (LayerZero message delivered). */
async function waitForAuctionOnBnb(
  contract: AuctionBidderRuntime["contract"],
  seriesId: number,
): Promise<number> {
  for (let i = 0; i < LZ_DELIVERY_MAX_ATTEMPTS; i++) {
    try {
      const stage = await contract.read.getAuctionStage([seriesId]);
      if (i > 0) console.log(`[commit-bids] Auction found on BNB after ${(i + 1) * (LZ_DELIVERY_POLL_MS / 1000)}s`);
      return stage as number;
    } catch {
      if (i === 0) console.log("[commit-bids] Waiting for auction on BNB (LayerZero delivery)...");
      await new Promise((r) => setTimeout(r, LZ_DELIVERY_POLL_MS));
    }
  }
  throw new Error(
    `Auction ${seriesId} not found on BNB after ~${(LZ_DELIVERY_MAX_ATTEMPTS * LZ_DELIVERY_POLL_MS) / 1000}s. ` +
      "LayerZero message may not have been delivered yet.",
  );
}

/** Poll until auction reaches a specific stage (waits for LZ delivery of stage transition). */
async function waitForStage(
  contract: AuctionBidderRuntime["contract"],
  seriesId: number,
  expectedStage: number,
  label: string,
): Promise<void> {
  for (let i = 0; i < LZ_DELIVERY_MAX_ATTEMPTS; i++) {
    const stage = await contract.read.getAuctionStage([seriesId]);
    if (stage === expectedStage) {
      if (i > 0) console.log(`[${label}] Stage ${expectedStage} reached after ${(i + 1) * (LZ_DELIVERY_POLL_MS / 1000)}s`);
      return;
    }
    if (i === 0) console.log(`[${label}] Waiting for stage ${expectedStage} (current: ${stage}, LayerZero delivery)...`);
    await new Promise((r) => setTimeout(r, LZ_DELIVERY_POLL_MS));
  }
  const current = await contract.read.getAuctionStage([seriesId]);
  throw new Error(
    `Auction did not reach stage ${expectedStage} after ~${(LZ_DELIVERY_MAX_ATTEMPTS * LZ_DELIVERY_POLL_MS) / 1000}s (current: ${current}).`,
  );
}

/**
 * Commit bids for multiple wallets.
 * Generates random bids and submits commit hashes.
 */
export async function runCommitBids(
  runtime: AuctionBidderRuntime,
  opts: CommitBidsOptions,
): Promise<CommitRecord[]> {
  const { contract, publicClient, chainId: runtimeChainId } = runtime;
  const chainId = BigInt(runtimeChainId);

  // Wait for auction to exist on BNB (LZ bridge message delivery)
  const stage = await waitForAuctionOnBnb(contract, opts.seriesId);

  // Verify auction is in commit stage (stage 0)
  if (stage !== 0) {
    throw new Error(`Auction not in CommittingBids stage (current: ${stage})`);
  }

  // Get auction info for defaults
  const auctionInfo = await contract.read.getAuctionInfo([opts.seriesId]);
  const floorPrice = auctionInfo.params.minIntexBidPrice;
  const minQuantity = Number(auctionInfo.params.minIntexBidQuantity);

  // Load existing commits
  const existingCommits = opts.commitsFile ? await loadCommitsFile(opts.commitsFile) : [];

  // Determine ranges
  const quantityRange: [number, number] = opts.quantityRange ?? [minQuantity, minQuantity + 5];
  const floorPriceFloat = Number(floorPrice) / STABLE_MULTIPLIER;
  const maxPriceFloat = Number(floorPrice + 10_000_000n) / STABLE_MULTIPLIER;
  const effectivePriceRange: [string, string] = opts.priceRange ?? [
    floorPriceFloat.toFixed(STABLE_DECIMALS),
    maxPriceFloat.toFixed(STABLE_DECIMALS),
  ];

  console.log("[commit-bids]", {
    seriesId: opts.seriesId,
    wallets: opts.wallets.length,
    quantityRange,
    priceRange: effectivePriceRange,
    floorPrice: floorPrice.toString(),
  });

  const newCommits: CommitRecord[] = [];

  for (const [index, wallet] of opts.wallets.entries()) {
    try {
      const bidderAddress = wallet.address as Address;
      const privateKey = wallet.privateKey as Hex;

      // Skip if already committed
      const existing = existingCommits.find(
        (c) => c.bidder === bidderAddress && c.seriesId === opts.seriesId,
      );
      if (existing) {
        console.log(`[#${index}] ${bidderAddress} already committed, skipping`);
        continue;
      }

      // Generate random bid
      const bid = generateRandomBid(quantityRange, opts.priceRange, floorPrice);
      const commitHash = await createCommitHash(
        opts.seriesId,
        bidderAddress,
        bid.quantity,
        bid.bidPrice,
        chainId,
        runtime.address,
        privateKey,
      );

      console.log(
        `[#${index}] ${bidderAddress} committing: qty=${bid.quantity}, price=${bid.priceFloat.toFixed(STABLE_DECIMALS)}`,
      );

      // Submit commit
      const account = privateKeyToAccount(privateKey) as unknown as WalletAccount;
      const tx = await contract.write.commitBid([opts.seriesId, commitHash], { account });
      await publicClient.waitForTransactionReceipt({ hash: tx });

      // Save record
      const record: CommitRecord = {
        bidder: bidderAddress,
        seriesId: opts.seriesId,
        commitHash,
        bidParams: {
          quantity: Number(bid.quantity),
          bidPrice: (Number(bid.bidPrice) / STABLE_MULTIPLIER).toFixed(STABLE_DECIMALS),
        },
        txHash: tx,
      };

      newCommits.push(record);
      console.log(`[#${index}] ${bidderAddress} committed, tx: ${tx}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      console.error(`[#${index}] Error for ${wallet.address}:`, message);
    }
  }

  // Save commits
  if (opts.commitsFile) {
    const otherCommits = existingCommits.filter((c) => c.seriesId !== opts.seriesId);
    const allCommits = [...otherCommits, ...newCommits];
    await saveCommitsFile(opts.commitsFile, allCommits);
    console.log(`[commit-bids] Saved ${allCommits.length} commits to ${opts.commitsFile} (${newCommits.length} new)`);
  }

  return newCommits;
}

/**
 * Reveal bids for multiple wallets.
 * Approves payment tokens and submits reveal signatures.
 */
export async function runRevealBids(
  runtime: AuctionBidderRuntime,
  opts: RevealBidsOptions,
): Promise<void> {
  const { contract, publicClient, chainId: runtimeChainId } = runtime;
  const chainId = BigInt(runtimeChainId);

  // Wait for auction to reach RevealingBids stage (LZ bridge message delivery)
  await waitForStage(contract, opts.seriesId, 1, "reveal-bids");

  // Load commits
  const commits = await loadCommitsFile(opts.commitsFile);
  if (commits.length === 0) {
    throw new Error(`Commits file not found or empty: ${opts.commitsFile}`);
  }

  const relevantCommits = commits.filter((c) => c.seriesId === opts.seriesId);
  const revealedBidders = new Set<string>();

  // Get EscrowAdapter address if not provided
  let escrowAdapterAddress = opts.escrowAdapterAddress;
  if (!escrowAdapterAddress && contract.read.escrowContract) {
    escrowAdapterAddress = await contract.read.escrowContract();
  }

  console.log("[reveal-bids]", {
    seriesId: opts.seriesId,
    commits: relevantCommits.length,
    escrowAdapter: escrowAdapterAddress,
    paymentToken: opts.paymentTokenAddress,
  });

  for (const [index, wallet] of opts.wallets.entries()) {
    try {
      const bidderAddress = wallet.address as Address;
      const privateKey = wallet.privateKey as Hex;

      // Find commit
      const commit = relevantCommits.find((c) => c.bidder === bidderAddress);
      if (!commit) {
        console.log(`[#${index}] ${bidderAddress} no commit found, skipping`);
        continue;
      }

      // Parse bid params
      const { quantity, bidPrice } = commit.bidParams;
      const quantityBig = BigInt(quantity);
      const bidPriceBig = BigInt(Math.floor(parseFloat(bidPrice) * STABLE_MULTIPLIER));
      const lockAmount = quantityBig * bidPriceBig;

      // Approve payment token
      if (escrowAdapterAddress && opts.paymentTokenAddress) {
        const approved = await approvePaymentToken(
          bidderAddress,
          privateKey,
          escrowAdapterAddress,
          opts.paymentTokenAddress,
          lockAmount,
          runtimeChainId,
        );
        if (!approved) {
          console.warn(`[#${index}] Approve failed for ${bidderAddress}, reveal may fail`);
        }
      }

      // Create signature and reveal
      const signature = await createRevealSignature(
        opts.seriesId,
        bidderAddress,
        quantityBig,
        bidPriceBig,
        chainId,
        runtime.address,
        privateKey,
      );

      console.log(`[#${index}] ${bidderAddress} revealing: qty=${quantity}, price=${bidPrice}`);

      const account = privateKeyToAccount(privateKey) as unknown as WalletAccount;
      const tx = await contract.write.revealBid(
        [opts.seriesId, quantityBig, bidPriceBig, chainId, signature],
        { account },
      );

      // Wait and check tx status
      const receipt = await publicClient.waitForTransactionReceipt({ hash: tx });
      if (receipt.status === "reverted") {
        console.error(`[#${index}] ${bidderAddress} reveal REVERTED, tx: ${tx}`);
        continue;
      }

      revealedBidders.add(bidderAddress);
      console.log(`[#${index}] ${bidderAddress} revealed, tx: ${tx}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      console.error(`[#${index}] Error for ${wallet.address}:`, message);
    }
  }

  // Update commits file
  const updatedCommits = commits.map((c) =>
    c.seriesId === opts.seriesId && revealedBidders.has(c.bidder) ? { ...c, revealed: true } : c,
  );
  await saveCommitsFile(opts.commitsFile, updatedCommits);
  console.log(`[reveal-bids] Updated ${opts.commitsFile} with ${revealedBidders.size} revealed bids`);
}
