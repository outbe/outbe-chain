// Settlement on Outbe — burn Issued, mint Settled.
//
// Flow:
//   1. approve stablecoins for IntexSettlement
//   2. settle(seriesId, intexHolder, amount) → pull stablecoins, deposit into the vault
//      provider, burn `amount` of the holder's Issued Intex and mint the same `amount` of
//      Settled (soulbound) Intex to the same holder.
//
// Settle is permitted while the series is `Qualified` (voluntary, user-driven) or `Called`
// (forced; deadline = calledAt + intexCallPeriod). Promis is no longer minted here — holders
// mint Promis themselves with `intex/mine.ts` against their Settled balance.
//
// Modes:
//   - Self-settle: holder pays and settles their own Intex
//   - Delegated settle: payer settles on behalf of intexHolder (requires authorizeSettler)

import type { Address, Hex, PublicClient as ViemPublicClient } from "viem";
import {
  createPublicClient,
  createWalletClient,
  formatUnits,
  getContract,
  http,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { OUTBE_CHAINS } from "../shared/layerzero.js";
import { getNetworkName } from "../shared/taskUtils.js";

// =============================================================================
// Minimal ABIs
// =============================================================================

const ERC20_ABI = [
  {
    inputs: [{ name: "owner", type: "address" }, { name: "spender", type: "address" }],
    name: "allowance",
    outputs: [{ type: "uint256" }],
    stateMutability: "view",
    type: "function",
  },
  {
    inputs: [{ name: "spender", type: "address" }, { name: "amount", type: "uint256" }],
    name: "approve",
    outputs: [{ type: "bool" }],
    stateMutability: "nonpayable",
    type: "function",
  },
  {
    inputs: [{ name: "account", type: "address" }],
    name: "balanceOf",
    outputs: [{ type: "uint256" }],
    stateMutability: "view",
    type: "function",
  },
  {
    inputs: [],
    name: "decimals",
    outputs: [{ type: "uint8" }],
    stateMutability: "view",
    type: "function",
  },
  {
    inputs: [],
    name: "symbol",
    outputs: [{ type: "string" }],
    stateMutability: "view",
    type: "function",
  },
] as const;

const INTEX_READ_ABI = [
  {
    inputs: [{ name: "seriesId", type: "uint32" }],
    name: "readData",
    outputs: [
      {
        components: [
          { name: "promisLoadMinor", type: "uint128" },
          { name: "costAmountMinor", type: "uint64" },
          { name: "floorPriceMinor", type: "uint64" },
          { name: "issuedAt", type: "uint32" },
          { name: "calledAt", type: "uint32" },
          { name: "intexCallPeriod", type: "uint32" },
          { name: "totalSupply", type: "uint32" },
          { name: "referenceCurrency", type: "uint16" },
          { name: "status", type: "uint8" },
          { name: "state", type: "uint8" },
          {
            name: "intexCallTrigger",
            type: "tuple",
            components: [
              { name: "windowDays", type: "uint16" },
              { name: "thresholdDays", type: "uint16" },
              { name: "callPriceMinor", type: "uint64" },
            ],
          },
        ],
        type: "tuple",
      },
    ],
    stateMutability: "view",
    type: "function",
  },
  {
    inputs: [{ name: "seriesId", type: "uint32" }],
    name: "issuedTokenId",
    outputs: [{ type: "uint256" }],
    stateMutability: "pure",
    type: "function",
  },
  {
    inputs: [{ name: "tokenId", type: "uint256" }],
    name: "getSeriesHoldersWithBalances",
    outputs: [
      { name: "holders", type: "address[]" },
      { name: "balances", type: "uint256[]" },
    ],
    stateMutability: "view",
    type: "function",
  },
  {
    inputs: [{ name: "account", type: "address" }, { name: "id", type: "uint256" }],
    name: "balanceOf",
    outputs: [{ type: "uint256" }],
    stateMutability: "view",
    type: "function",
  },
] as const;

const SETTLEMENT_ABI = [
  {
    inputs: [
      { name: "seriesId", type: "uint32" },
      { name: "intexHolder", type: "address" },
      { name: "amount", type: "uint256" },
    ],
    name: "settle",
    outputs: [],
    stateMutability: "nonpayable",
    type: "function",
  },
  {
    inputs: [{ name: "seriesId", type: "uint32" }, { name: "settler", type: "address" }],
    name: "authorizeSettler",
    outputs: [],
    stateMutability: "nonpayable",
    type: "function",
  },
  {
    inputs: [],
    name: "paymentToken",
    outputs: [{ type: "address" }],
    stateMutability: "view",
    type: "function",
  },
] as const;

const PROMIS_ABI = [
  {
    inputs: [{ name: "account", type: "address" }],
    name: "balanceOf",
    outputs: [{ type: "uint256" }],
    stateMutability: "view",
    type: "function",
  },
  {
    inputs: [],
    name: "decimals",
    outputs: [{ type: "uint8" }],
    stateMutability: "view",
    type: "function",
  },
] as const;

const INTEX_STATE_NAMES: Record<number, string> = {
  0: "Issued",
  1: "Qualified",
  2: "Called",
};

// =============================================================================
// Types
// =============================================================================

export interface SettleOpts {
  settlementAddress: Address;
  intexAddress: Address;
  promisAddress?: Address;
  seriesId: number;
  intexHolder?: Address;
}

export interface SettlePreviewResult {
  seriesId: number;
  state: string;
  costAmountMinor: bigint;
  promisLoadMinor: bigint;
  /** Effective call deadline (calledAt + intexCallPeriod). null if the series is not yet `Called`. */
  callDeadline: Date | null;
  holderBalance: bigint;
  settleAmount: bigint;
  assetsRequired: bigint;
  paymentTokenSymbol: string;
  paymentTokenDecimals: number;
  payerTokenBalance: bigint;
  currentAllowance: bigint;
  needsApproval: boolean;
  /** Promis amount the holder will be eligible to mine after settle (= amount * promisLoadMinor). */
  promisToMint: bigint;
}

// =============================================================================
// Outbe Client Factory
// =============================================================================

export function createOutbeClients(networkName: string) {
  const outbeNetworks = Object.keys(OUTBE_CHAINS);
  if (!outbeNetworks.includes(networkName)) {
    throw new Error(`Not an Outbe network: ${networkName}. Expected one of: ${outbeNetworks.join(", ")}`);
  }
  const chain = OUTBE_CHAINS[networkName as keyof typeof OUTBE_CHAINS];
  const rpc = process.env.OUTBE_RPC_URL ?? chain.rpcUrls.default.http[0];
  const pk = process.env.OUTBE_PRIVATE_KEY;
  if (!pk) throw new Error("OUTBE_PRIVATE_KEY required for Outbe networks");
  const account = privateKeyToAccount(pk as `0x${string}`);
  const transport = http(rpc);
  const publicClient = createPublicClient({ chain, transport });
  const walletClient = createWalletClient({ account, chain, transport });
  return { publicClient, walletClient, account };
}

// =============================================================================
// Preview Settle
// =============================================================================

export async function previewSettle(
  publicClient: ViemPublicClient,
  payer: Address,
  opts: SettleOpts,
): Promise<SettlePreviewResult> {
  const { settlementAddress, intexAddress, seriesId } = opts;

  const data = await publicClient.readContract({
    address: intexAddress,
    abi: INTEX_READ_ABI,
    functionName: "readData",
    args: [seriesId],
  });
  const { promisLoadMinor, costAmountMinor, calledAt, intexCallPeriod, state } = data as {
    promisLoadMinor: bigint;
    costAmountMinor: bigint;
    calledAt: number;
    intexCallPeriod: number;
    state: number;
  };
  const isCalled = state === 2;
  const callDeadline = isCalled && calledAt > 0
    ? new Date((Number(calledAt) + Number(intexCallPeriod)) * 1000)
    : null;

  const intexHolder = opts.intexHolder ?? payer;
  const tokenId = await publicClient.readContract({
    address: intexAddress,
    abi: INTEX_READ_ABI,
    functionName: "issuedTokenId",
    args: [seriesId],
  }) as bigint;
  const holderBalance = await publicClient.readContract({
    address: intexAddress,
    abi: INTEX_READ_ABI,
    functionName: "balanceOf",
    args: [intexHolder, tokenId],
  }) as bigint;

  const paymentTokenAddress = await publicClient.readContract({
    address: settlementAddress,
    abi: SETTLEMENT_ABI,
    functionName: "paymentToken",
  }) as Address;

  // Settlement cost is computed off-chain (matches IntexSettlement._settlementCost).
  const settleAmount = holderBalance;
  const assetsRequired = costAmountMinor * settleAmount;

  const [paymentSymbol, paymentDecimals, payerTokenBalance, currentAllowance] = await Promise.all([
    publicClient.readContract({ address: paymentTokenAddress, abi: ERC20_ABI, functionName: "symbol" }) as Promise<string>,
    publicClient.readContract({ address: paymentTokenAddress, abi: ERC20_ABI, functionName: "decimals" }) as Promise<number>,
    publicClient.readContract({ address: paymentTokenAddress, abi: ERC20_ABI, functionName: "balanceOf", args: [payer] }) as Promise<bigint>,
    publicClient.readContract({ address: paymentTokenAddress, abi: ERC20_ABI, functionName: "allowance", args: [payer, settlementAddress] }) as Promise<bigint>,
  ]);

  return {
    seriesId,
    state: INTEX_STATE_NAMES[state] ?? `Unknown(${state})`,
    costAmountMinor,
    promisLoadMinor,
    callDeadline,
    holderBalance,
    settleAmount,
    assetsRequired,
    paymentTokenSymbol: paymentSymbol,
    paymentTokenDecimals: paymentDecimals,
    payerTokenBalance,
    currentAllowance,
    needsApproval: currentAllowance < assetsRequired,
    promisToMint: settleAmount * promisLoadMinor,
  };
}

// =============================================================================
// Execute Settle
// =============================================================================

export interface SettleResult {
  approveTx?: Hex;
  settleTx: Hex;
  settledAmount: bigint;
  paymentAmount: bigint;
  promisMinted: bigint;
}

export async function executeSettle(
  hre: unknown,
  opts: SettleOpts,
): Promise<SettleResult> {
  const networkName = getNetworkName(hre);

  const { publicClient, walletClient, account } = createOutbeClients(networkName);
  const payer = account.address;
  const preview = await previewSettle(publicClient, payer, opts);
  const latestBlock = await publicClient.getBlock();
  const chainNowMs = Number(latestBlock.timestamp) * 1000;

  console.log("\n=== Settlement Preview ===");
  console.log(`  Series state:        ${preview.state}`);
  console.log(`  Cost amount:         ${preview.costAmountMinor}`);
  console.log(`  Promis load:         ${preview.promisLoadMinor}`);
  console.log(
    `  Call deadline:       ${preview.callDeadline ? preview.callDeadline.toISOString() : "n/a (not Called)"}`,
  );
  console.log(`  Holder balance:      ${preview.holderBalance} Intex`);
  console.log(`  Settle amount:       ${preview.settleAmount} Intex`);
  console.log(
    `  Assets required:     ${formatUnits(preview.assetsRequired, preview.paymentTokenDecimals)} ${preview.paymentTokenSymbol}`,
  );
  console.log(
    `  Token balance:       ${formatUnits(preview.payerTokenBalance, preview.paymentTokenDecimals)} ${preview.paymentTokenSymbol}`,
  );
  console.log(
    `  Token allowance:     ${formatUnits(preview.currentAllowance, preview.paymentTokenDecimals)} ${preview.paymentTokenSymbol}`,
  );
  console.log(`  Promis (post-mine):  ${preview.promisToMint}`);

  if (preview.state !== "Qualified" && preview.state !== "Called") {
    throw new Error(
      `Series is not eligible for settle (current: ${preview.state}). Expected Qualified or Called.`,
    );
  }
  if (preview.state === "Called" && preview.callDeadline && preview.callDeadline.getTime() < chainNowMs) {
    throw new Error(`Call deadline has passed (${preview.callDeadline.toISOString()}). Cannot settle.`);
  }
  if (preview.settleAmount === 0n) {
    throw new Error("Settle amount is 0. Nothing to settle.");
  }
  if (preview.payerTokenBalance < preview.assetsRequired) {
    throw new Error(
      `Insufficient stablecoin balance: have ${formatUnits(preview.payerTokenBalance, preview.paymentTokenDecimals)}, ` +
      `need ${formatUnits(preview.assetsRequired, preview.paymentTokenDecimals)} ${preview.paymentTokenSymbol}`,
    );
  }

  let approveTx: Hex | undefined;

  if (preview.currentAllowance < preview.assetsRequired) {
    console.log(`\n[approve] Approving ${formatUnits(preview.assetsRequired, preview.paymentTokenDecimals)} ${preview.paymentTokenSymbol}...`);
    const paymentTokenAddress = await publicClient.readContract({
      address: opts.settlementAddress,
      abi: SETTLEMENT_ABI,
      functionName: "paymentToken",
    }) as Address;

    const tokenContract = getContract({
      address: paymentTokenAddress,
      abi: ERC20_ABI,
      client: { public: publicClient, wallet: walletClient },
    });
    // Zero allowance first for USDT-style tokens that require it
    if (preview.currentAllowance > 0n) {
      const zeroTx = await tokenContract.write.approve([opts.settlementAddress, 0n]);
      await publicClient.waitForTransactionReceipt({ hash: zeroTx });
      console.log(`[approve] Zeroed existing allowance. Tx: ${zeroTx}`);
    }
    approveTx = await tokenContract.write.approve([opts.settlementAddress, preview.assetsRequired]);
    await publicClient.waitForTransactionReceipt({ hash: approveTx });
    console.log(`[approve] Done. Tx: ${approveTx}`);
  } else {
    console.log("\n[approve] Sufficient allowance, skipping approve.");
  }

  const intexHolder = opts.intexHolder ?? payer;
  const settlementContract = getContract({
    address: opts.settlementAddress,
    abi: SETTLEMENT_ABI,
    client: { public: publicClient, wallet: walletClient },
  });

  console.log(`\n[settle] Settling ${preview.settleAmount} Intex (full balance) for holder ${intexHolder}...`);
  const settleTx = await settlementContract.write.settle([
    opts.seriesId,
    intexHolder,
    preview.settleAmount,
  ]);
  await publicClient.waitForTransactionReceipt({ hash: settleTx });
  console.log(`[settle] Done. Tx: ${settleTx}`);

  if (opts.promisAddress) {
    const promisBalance = await publicClient.readContract({
      address: opts.promisAddress,
      abi: PROMIS_ABI,
      functionName: "balanceOf",
      args: [payer],
    }) as bigint;
    console.log(`\n[result] Current Promis balance of payer: ${promisBalance}`);
    console.log("[result] Promis is not minted by settle. Run intex/mine.ts to mint Promis from Settled balance.");
  }

  console.log("\n=== Settlement Complete ===");
  console.log({
    settledAmount: preview.settleAmount.toString(),
    paymentAmount: formatUnits(preview.assetsRequired, preview.paymentTokenDecimals) + ` ${preview.paymentTokenSymbol}`,
    promisEligible: preview.promisToMint.toString(),
    settleTx,
  });

  return {
    approveTx,
    settleTx,
    settledAmount: preview.settleAmount,
    paymentAmount: preview.assetsRequired,
    promisMinted: preview.promisToMint,
  };
}
