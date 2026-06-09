/**
 * Fund bidder wallets on BSC Testnet with tBNB and/or USDC (MockERC20),
 * and update on-chain balances in the wallets JSON file.
 *
 * Reads BSC_TESTNET_RPC_URL and BSC_TESTNET_PRIVATE_KEY from .env
 *
 * Usage:
 *   npx tsx scripts/fundBidderWallets.ts fund tbnb [amount]
 *   npx tsx scripts/fundBidderWallets.ts fund usdc [amount]
 *   npx tsx scripts/fundBidderWallets.ts fund all  [tbnbAmount] [usdcAmount]
 *   npx tsx scripts/fundBidderWallets.ts update-balances
 *
 * If [amount] is provided, each wallet is topped up to that amount.
 * Otherwise, startBalance from the JSON file is used.
 */

import "dotenv/config";
import {
  createPublicClient,
  createWalletClient,
  http,
  parseUnits,
  formatUnits,
  formatEther,
  parseEther,
  parseAbi,
  type Address,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { bscTestnet } from "viem/chains";
import { readFileSync, writeFileSync } from "fs";
import path from "path";

// =============================================================================
// Constants
// =============================================================================

const WALLETS_PATH = path.join(process.cwd(), "data", "bidders-generated-bsc-testnet-wallets.json");
const USDC_ADDRESS: Address = "0xe577886C94eF6F87632224c22F1276e15b9A96E3";
const BSC_RPC_URL = process.env.BSC_TESTNET_RPC_URL ?? "https://bsc-testnet.publicnode.com";

const ERC20_ABI = parseAbi([
  "function transfer(address to, uint256 amount) returns (bool)",
  "function balanceOf(address account) view returns (uint256)",
  "function decimals() view returns (uint8)",
  "function symbol() view returns (string)",
]);

// =============================================================================
// Types
// =============================================================================

interface WalletEntry {
  address: string;
  privateKey: string;
  startBalance: {
    tBNB: string;
    USDC: string;
  };
  currentBalance?: {
    tBNB: string;
    USDC: string;
  };
}

// =============================================================================
// Helpers
// =============================================================================

function loadWallets(): WalletEntry[] {
  return JSON.parse(readFileSync(WALLETS_PATH, "utf8")) as WalletEntry[];
}

function saveWallets(wallets: WalletEntry[]): void {
  writeFileSync(WALLETS_PATH, JSON.stringify(wallets, null, 2) + "\n");
}

function requirePrivateKey(): `0x${string}` {
  const pk = process.env.BSC_TESTNET_PRIVATE_KEY;
  if (!pk) {
    console.error("Error: Set BSC_TESTNET_PRIVATE_KEY in .env");
    process.exit(1);
  }
  return (pk.startsWith("0x") ? pk : `0x${pk}`) as `0x${string}`;
}

// =============================================================================
// Fund tBNB — native transfer from sender to each wallet
// =============================================================================

async function fundTBNB(wallets: WalletEntry[], amountOverride?: string): Promise<void> {
  const pk = requirePrivateKey();
  const account = privateKeyToAccount(pk);
  const transport = http(BSC_RPC_URL, { timeout: 15_000 });
  const publicClient = createPublicClient({ chain: bscTestnet, transport });
  const walletClient = createWalletClient({ account, chain: bscTestnet, transport });

  const senderBalance = await publicClient.getBalance({ address: account.address });
  console.log(`[fund-tbnb] Sender ${account.address} balance: ${formatEther(senderBalance)} tBNB`);
  if (amountOverride) console.log(`[fund-tbnb] Amount override: ${amountOverride} tBNB per wallet`);

  let totalNeeded = 0n;
  const transfers: { address: Address; amount: bigint }[] = [];

  for (const w of wallets) {
    const target = w.address as Address;
    const desired = parseEther(amountOverride ?? w.startBalance.tBNB);
    const current = await publicClient.getBalance({ address: target });
    const desiredStr = amountOverride ?? w.startBalance.tBNB;

    if (current >= desired) {
      console.log(`  ${target} — already has ${formatEther(current)} tBNB (need ${desiredStr}), skipping`);
      continue;
    }

    const deficit = desired - current;
    transfers.push({ address: target, amount: deficit });
    totalNeeded += deficit;
  }

  if (transfers.length === 0) {
    console.log("[fund-tbnb] All wallets already funded");
    return;
  }

  const gasReserve = parseEther("0.01");
  if (senderBalance < totalNeeded + gasReserve) {
    console.error(
      `[fund-tbnb] Insufficient sender balance. Need ~${formatEther(totalNeeded + gasReserve)} tBNB, have ${formatEther(senderBalance)}`,
    );
    process.exit(1);
  }

  console.log(`[fund-tbnb] Sending tBNB to ${transfers.length} wallets (total: ${formatEther(totalNeeded)})...\n`);

  for (const { address, amount } of transfers) {
    const hash = await walletClient.sendTransaction({ to: address, value: amount });
    await publicClient.waitForTransactionReceipt({ hash });
    console.log(`  ${address} +${formatEther(amount)} tBNB  tx: ${hash}`);
  }

  console.log("\n[fund-tbnb] Done");
}

// =============================================================================
// Fund USDC — transfer ERC20 from sender to each wallet
// =============================================================================

async function fundUSDC(wallets: WalletEntry[], amountOverride?: string): Promise<void> {
  const pk = requirePrivateKey();
  const account = privateKeyToAccount(pk);
  const transport = http(BSC_RPC_URL, { timeout: 15_000 });
  const publicClient = createPublicClient({ chain: bscTestnet, transport });
  const walletClient = createWalletClient({ account, chain: bscTestnet, transport });

  const decimals = await publicClient.readContract({
    address: USDC_ADDRESS,
    abi: ERC20_ABI,
    functionName: "decimals",
  });
  const symbol = await publicClient.readContract({
    address: USDC_ADDRESS,
    abi: ERC20_ABI,
    functionName: "symbol",
  });
  const senderBalance = await publicClient.readContract({
    address: USDC_ADDRESS,
    abi: ERC20_ABI,
    functionName: "balanceOf",
    args: [account.address],
  });

  console.log(`[fund-usdc] Token: ${symbol} (${decimals} decimals) at ${USDC_ADDRESS}`);
  console.log(`[fund-usdc] Sender ${account.address} balance: ${formatUnits(senderBalance, decimals)} ${symbol}`);
  if (amountOverride) console.log(`[fund-usdc] Amount override: ${amountOverride} ${symbol} per wallet`);
  console.log("");

  let totalNeeded = 0n;
  const transfers: { address: Address; amount: bigint }[] = [];

  for (const w of wallets) {
    const target = w.address as Address;
    const desiredStr = amountOverride ?? w.startBalance.USDC;
    const desired = parseUnits(desiredStr, decimals);
    const current = await publicClient.readContract({
      address: USDC_ADDRESS,
      abi: ERC20_ABI,
      functionName: "balanceOf",
      args: [target],
    });

    if (current >= desired) {
      console.log(
        `  ${target} — already has ${formatUnits(current, decimals)} ${symbol} (need ${desiredStr}), skipping`,
      );
      continue;
    }

    const deficit = desired - current;
    transfers.push({ address: target, amount: deficit });
    totalNeeded += deficit;
  }

  if (transfers.length === 0) {
    console.log("[fund-usdc] All wallets already funded");
    return;
  }

  if (senderBalance < totalNeeded) {
    console.error(
      `[fund-usdc] Insufficient sender balance. Need ${formatUnits(totalNeeded, decimals)} ${symbol}, have ${formatUnits(senderBalance, decimals)}`,
    );
    process.exit(1);
  }

  console.log(`\n[fund-usdc] Transferring to ${transfers.length} wallets (total: ${formatUnits(totalNeeded, decimals)} ${symbol})...\n`);

  for (const { address, amount } of transfers) {
    const hash = await walletClient.writeContract({
      address: USDC_ADDRESS,
      abi: ERC20_ABI,
      functionName: "transfer",
      args: [address, amount],
    });
    await publicClient.waitForTransactionReceipt({ hash });
    console.log(`  ${address} +${formatUnits(amount, decimals)} ${symbol}  tx: ${hash}`);
  }

  console.log(`\n[fund-usdc] Done, transferred to ${transfers.length} wallets`);
}

// =============================================================================
// Update balances — read on-chain balances and save to JSON
// =============================================================================

async function updateBalances(wallets: WalletEntry[]): Promise<void> {
  const transport = http(BSC_RPC_URL, { timeout: 15_000 });
  const publicClient = createPublicClient({ chain: bscTestnet, transport });

  const decimals = await publicClient.readContract({
    address: USDC_ADDRESS,
    abi: ERC20_ABI,
    functionName: "decimals",
  });

  console.log(`[update-balances] Reading on-chain balances for ${wallets.length} wallets...\n`);

  for (const w of wallets) {
    const addr = w.address as Address;

    const [tbnb, usdc] = await Promise.all([
      publicClient.getBalance({ address: addr }),
      publicClient.readContract({
        address: USDC_ADDRESS,
        abi: ERC20_ABI,
        functionName: "balanceOf",
        args: [addr],
      }),
    ]);

    w.currentBalance = {
      tBNB: formatEther(tbnb),
      USDC: formatUnits(usdc, decimals),
    };

    console.log(`  ${addr}  tBNB: ${w.currentBalance.tBNB}  USDC: ${w.currentBalance.USDC}`);
  }

  saveWallets(wallets);
  console.log(`\n[update-balances] Saved to ${WALLETS_PATH}`);
}

// =============================================================================
// Main
// =============================================================================

function validateAmount(value: string, label: string): void {
  if (isNaN(parseFloat(value)) || parseFloat(value) <= 0) {
    console.error(`Error: Invalid ${label} amount "${value}". Provide a positive number.`);
    process.exit(1);
  }
}

async function main() {
  const command = process.argv[2];
  const token = process.argv[3]?.toLowerCase();

  const wallets = loadWallets();

  if (command === "fund") {
    if (token === "tbnb") {
      const amount = process.argv[4];
      if (amount) validateAmount(amount, "tBNB");
      await fundTBNB(wallets, amount);
    } else if (token === "usdc") {
      const amount = process.argv[4];
      if (amount) validateAmount(amount, "USDC");
      await fundUSDC(wallets, amount);
    } else if (token === "all") {
      const tbnbAmount = process.argv[4];
      const usdcAmount = process.argv[5];
      if (tbnbAmount) validateAmount(tbnbAmount, "tBNB");
      if (usdcAmount) validateAmount(usdcAmount, "USDC");
      await fundTBNB(wallets, tbnbAmount);
      console.log("");
      await fundUSDC(wallets, usdcAmount);
    } else {
      console.error("Usage:");
      console.error("  tsx scripts/fundBidderWallets.ts fund tbnb [amount]");
      console.error("  tsx scripts/fundBidderWallets.ts fund usdc [amount]");
      console.error("  tsx scripts/fundBidderWallets.ts fund all  [tbnbAmount] [usdcAmount]");
      process.exit(1);
    }
  } else if (command === "update-balances") {
    await updateBalances(wallets);
  } else {
    console.error("Usage:");
    console.error("  tsx scripts/fundBidderWallets.ts fund tbnb [amount]");
    console.error("  tsx scripts/fundBidderWallets.ts fund usdc [amount]");
    console.error("  tsx scripts/fundBidderWallets.ts fund all  [tbnbAmount] [usdcAmount]");
    console.error("  tsx scripts/fundBidderWallets.ts update-balances");
    console.error("");
    console.error("If amounts are omitted, startBalance from the JSON file is used.");
    process.exit(1);
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
