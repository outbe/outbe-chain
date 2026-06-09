// Qualified Full Flow Task
//
// User-driven path that ends in Promis. Runs on BSC for the first two steps
// (markQualified + bridge to Outbe) and switches to an Outbe RPC for the last
// two (settle + mine). Mirrors the structure of `settlement-full-flow` but for
// the voluntary qualifier route rather than the system bridge.
//
// Phase Q1 — markQualified  (BSC, RELAYER_ROLE)        Issued → Qualified
// Phase Q2 — bridge          (BSC, user)                Issued → Outbe via ONFT1155Adapter
// Phase Q3 — settle          (Outbe, user)              burn Issued + mint Settled
// Phase Q4 — mine Promis     (Outbe, user, optional)    burn Settled + mint Promis
//
// Run with `--network bscTestnet` (or bsc) — that is where mark + bridge happen.
// The `--outbe-network` arg picks which Outbe RPC the settle/mine phases connect to.

import { task } from "hardhat/config";
import { http, createPublicClient, createWalletClient, formatUnits } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import type { Address, Hex } from "viem";

import { markSeriesQualified } from "../../scripts/intex/qualify.js";
import { bridgeIntexToOutbe } from "../../scripts/intex/bridgeToOutbe.js";
import { executeSettle, createOutbeClients } from "../../scripts/intex/settle.js";
import { minePromis, type MineClient } from "../../scripts/intex/mine.js";
import { resolveSeriesId } from "../../scripts/shared/auctionId.js";
import { lazy, toOptional, getNetworkName } from "../../scripts/shared/taskUtils.js";
import { promptForContinue } from "../../scripts/shared/prompt.js";

// =============================================================================
// Types
// =============================================================================

interface QualifiedFullFlowTaskArgs {
  intexBscContract?: string;
  intexOutbeContract?: string;
  onftAdapterContract?: string;
  settlementContract?: string;
  promisContract?: string;
  outbeEid?: string;
  outbeNetwork?: string;
  series?: string;
  amount?: string;
  intexHolder?: string;
  skipMark?: string;
  skipBridge?: string;
  skipSettle?: string;
  skipMine?: string;
}

interface ConnectableHre {
  network: {
    connect: () => Promise<{
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      viem: any;
    }>;
  };
}

const SETTLED_TOKEN_READ_ABI = [
  {
    inputs: [{ name: "seriesId", type: "uint32" }],
    name: "settledTokenId",
    outputs: [{ type: "uint256" }],
    stateMutability: "pure",
    type: "function",
  },
  {
    inputs: [
      { name: "account", type: "address" },
      { name: "id", type: "uint256" },
    ],
    name: "balanceOf",
    outputs: [{ type: "uint256" }],
    stateMutability: "view",
    type: "function",
  },
] as const;

// =============================================================================
// Helpers
// =============================================================================

function requireAddress(value: string | undefined, name: string): `0x${string}` {
  if (!value) throw new Error(`Missing required parameter: --${name}`);
  return value as `0x${string}`;
}

function parseBoolean(value: string | undefined): boolean {
  if (!value) return false;
  return ["true", "1", "yes"].includes(value.toLowerCase());
}

async function pollForBalance(
  publicClient: ReturnType<typeof createOutbeClients>["publicClient"],
  intexAddress: Address,
  holder: Address,
  tokenId: bigint,
  expected: bigint,
  timeoutMs = 180_000,
): Promise<bigint> {
  const start = Date.now();
  let last = 0n;
  while (Date.now() - start < timeoutMs) {
    last = (await publicClient.readContract({
      address: intexAddress,
      abi: SETTLED_TOKEN_READ_ABI,
      functionName: "balanceOf",
      args: [holder, tokenId],
    })) as bigint;
    if (last >= expected) return last;
    await new Promise((r) => setTimeout(r, 5_000));
  }
  return last;
}

// =============================================================================
// Action
// =============================================================================

const qualifiedFullFlowAction = async (args: QualifiedFullFlowTaskArgs, hre: unknown) => {
  const intexBscContract = requireAddress(toOptional(args.intexBscContract), "intex-bsc-contract");
  const intexOutbeContract = requireAddress(toOptional(args.intexOutbeContract), "intex-outbe-contract");
  const onftAdapterContract = requireAddress(toOptional(args.onftAdapterContract), "onft-adapter-contract");
  const settlementContract = requireAddress(toOptional(args.settlementContract), "settlement-contract");
  const promisContract = toOptional(args.promisContract) as `0x${string}` | undefined;
  const outbeEidStr = toOptional(args.outbeEid);
  if (!outbeEidStr) throw new Error("Missing required parameter: --outbe-eid");
  const outbeEid = Number(outbeEidStr);
  const outbeNetwork = toOptional(args.outbeNetwork) || "outbeTestnet";

  const seriesId = resolveSeriesId(toOptional(args.series));
  const amountStr = toOptional(args.amount);
  if (!amountStr) throw new Error("Missing required parameter: --amount");
  const amount = BigInt(amountStr);

  const skipMark = parseBoolean(args.skipMark);
  const skipBridge = parseBoolean(args.skipBridge);
  const skipSettle = parseBoolean(args.skipSettle);
  const skipMine = parseBoolean(args.skipMine);

  const networkName = getNetworkName(hre);
  const { viem } = await (hre as ConnectableHre).network.connect();
  const bscPublicClient = await viem.getPublicClient();
  const wallets = await viem.getWalletClients();
  const bscWallet = wallets[0];
  if (!bscWallet) throw new Error("No BSC wallet available — set the network's PRIVATE_KEY env var");
  const holderOverride = toOptional(args.intexHolder) as `0x${string}` | undefined;
  const holder: Address = holderOverride ?? bscWallet.account.address;

  console.log("\n=== Qualified Full Flow ===");
  console.log(`  BSC network:        ${networkName}`);
  console.log(`  Outbe network:      ${outbeNetwork}`);
  console.log(`  Series ID:          ${seriesId}`);
  console.log(`  Amount:             ${amount.toString()}`);
  console.log(`  Holder:             ${holder}`);
  console.log(`  IntexNFT1155 (BSC): ${intexBscContract}`);
  console.log(`  ONFT1155Adapter:    ${onftAdapterContract}`);
  console.log(`  IntexNFT1155 (Out): ${intexOutbeContract}`);
  console.log(`  IntexSettlement:    ${settlementContract}`);
  console.log(`  Promis:             ${promisContract ?? "(not set — Phase Q4 will be skipped)"}`);

  // ---------------------------------------------------------------------------
  // Phase Q1 — markQualified on BSC
  // ---------------------------------------------------------------------------
  if (skipMark) {
    console.log("\n[Q1/4] Skipping markQualified (--skip-mark).");
  } else {
    console.log("\n[Q1/4] markQualified on BSC IntexNFT1155...");
    await markSeriesQualified(
      { publicClient: bscPublicClient, walletClient: bscWallet },
      { intexAddress: intexBscContract, seriesId },
    );
  }

  // ---------------------------------------------------------------------------
  // Phase Q2 — bridge Issued from BSC to Outbe via ONFT1155Adapter
  // ---------------------------------------------------------------------------
  if (skipBridge) {
    console.log("\n[Q2/4] Skipping bridge (--skip-bridge).");
  } else {
    await promptForContinue("\nReady to bridge. Press Enter to continue...");
    console.log(`\n[Q2/4] Bridging ${amount.toString()} Intex from BSC to Outbe (eid ${outbeEid})...`);
    await bridgeIntexToOutbe(
      { publicClient: bscPublicClient, walletClient: bscWallet },
      {
        adapterAddress: onftAdapterContract,
        dstEid: outbeEid,
        seriesId,
        amount,
      },
    );
  }

  // ---------------------------------------------------------------------------
  // Switch to Outbe context for the remaining phases
  // ---------------------------------------------------------------------------
  const outbe = createOutbeClients(outbeNetwork);
  const tokenId = BigInt(seriesId);

  if (!skipBridge) {
    console.log(`\n[Q2/4] Waiting for LZ delivery on Outbe (max 3 min)...`);
    const delivered = await pollForBalance(
      outbe.publicClient,
      intexOutbeContract,
      holder,
      tokenId,
      amount,
    );
    console.log(`  Outbe Issued balance now: ${delivered.toString()} (expected ≥ ${amount.toString()})`);
    if (delivered < amount) {
      console.log("  ⚠️  LZ delivery incomplete; subsequent phases may revert. Check the bridge or retry.");
    }
  }

  // ---------------------------------------------------------------------------
  // Phase Q3 — settle on Outbe
  // ---------------------------------------------------------------------------
  if (skipSettle) {
    console.log("\n[Q3/4] Skipping settle (--skip-settle).");
  } else {
    await promptForContinue("\nReady to settle. Press Enter to continue...");
    console.log("\n[Q3/4] settle on Outbe IntexSettlement...");
    // Reuse executeSettle; it reads the network name from hre globally — but here
    // we're still on the BSC hardhat connect. executeSettle internally builds an
    // OUTBE viem client from `getNetworkName(hre)` which would resolve to the BSC
    // network. Bypass that by passing the outbe-network through process.env.
    process.env.NETWORK = outbeNetwork; // executeSettle reads via getNetworkName fallback
    await executeSettle(hre, {
      settlementAddress: settlementContract,
      intexAddress: intexOutbeContract,
      promisAddress: promisContract,
      seriesId,
    });
  }

  // ---------------------------------------------------------------------------
  // Phase Q4 — mine Promis on Outbe (optional)
  // ---------------------------------------------------------------------------
  if (skipMine) {
    console.log("\n[Q4/4] Skipping mine (--skip-mine).");
  } else if (!promisContract) {
    console.log("\n[Q4/4] Skipping mine — `--promis-contract` not provided.");
  } else {
    await promptForContinue("\nReady to mine Promis. Press Enter to continue...");
    console.log("\n[Q4/4] minePromis on Outbe Promis...");

    const settledId = await outbe.publicClient.readContract({
      address: intexOutbeContract,
      abi: SETTLED_TOKEN_READ_ABI,
      functionName: "settledTokenId",
      args: [seriesId],
    });
    const settledBalance = await outbe.publicClient.readContract({
      address: intexOutbeContract,
      abi: SETTLED_TOKEN_READ_ABI,
      functionName: "balanceOf",
      args: [outbe.account.address, settledId],
    });
    if (settledBalance === 0n) {
      console.log(`  Settled balance is 0 for ${outbe.account.address}; nothing to mine.`);
    } else {
      await minePromis(
        { publicClient: outbe.publicClient, walletClient: outbe.walletClient } as unknown as MineClient,
        {
          settlementAddress: settlementContract,
          promisAddress: promisContract,
          intexAddress: intexOutbeContract,
          seriesId,
          amount: settledBalance,
        },
      );
    }
  }

  console.log("\n=== Qualified flow complete ===");
};

// =============================================================================
// Task Definition
// =============================================================================

const qualifiedFullFlow = task(
  "qualified-full-flow",
  "Voluntary path: markQualified (BSC) → bridge to Outbe → settle (Outbe) → mine Promis (Outbe). Run with --network bscTestnet (or bsc).",
)
  .addOption({ name: "intexBscContract", description: "IntexNFT1155 contract on BSC (required)", defaultValue: "" })
  .addOption({ name: "intexOutbeContract", description: "IntexNFT1155 contract on Outbe (required)", defaultValue: "" })
  .addOption({ name: "onftAdapterContract", description: "ONFT1155Adapter contract on BSC (required)", defaultValue: "" })
  .addOption({ name: "settlementContract", description: "IntexSettlement contract on Outbe (required)", defaultValue: "" })
  .addOption({
    name: "promisContract",
    description: "Promis contract on Outbe. When set, Phase Q4 (mine) runs after Phase Q3.",
    defaultValue: "",
  })
  .addOption({
    name: "outbeEid",
    description: "Outbe LayerZero EID (e.g. 40812 for outbeTestnet, 40712 for outbeDevnet, 40512 for outbePrivnet)",
    defaultValue: "",
  })
  .addOption({
    name: "outbeNetwork",
    description: "Outbe network for settle/mine RPC (one of outbeTestnet, outbeDevnet, outbePrivnet)",
    defaultValue: "outbeTestnet",
  })
  .addOption({ name: "series", description: "Series in yyyymmdd format (e.g. 20260501)", defaultValue: "" })
  .addOption({
    name: "amount",
    description: "Amount of Issued Intex to mark/bridge/settle (units, not wei)",
    defaultValue: "",
  })
  .addOption({
    name: "intexHolder",
    description: "Override holder address (default: caller)",
    defaultValue: "",
  })
  .addOption({ name: "skipMark", description: "Skip Phase Q1 (markQualified)", defaultValue: "false" })
  .addOption({ name: "skipBridge", description: "Skip Phase Q2 (bridge)", defaultValue: "false" })
  .addOption({ name: "skipSettle", description: "Skip Phase Q3 (settle)", defaultValue: "false" })
  .addOption({ name: "skipMine", description: "Skip Phase Q4 (mine Promis)", defaultValue: "false" })
  .setAction(lazy(qualifiedFullFlowAction));

// =============================================================================
// Phase Q2 standalone — bridge Issued Intex from BSC to Outbe via ONFT1155Adapter
// =============================================================================

interface BridgeToOutbeTaskArgs {
  onftAdapterContract?: string;
  outbeEid?: string;
  series?: string;
  amount?: string;
  recipient?: string;
}

const bridgeToOutbeAction = async (args: BridgeToOutbeTaskArgs, hre: unknown) => {
  const adapterAddress = requireAddress(toOptional(args.onftAdapterContract), "onft-adapter-contract");
  const outbeEidStr = toOptional(args.outbeEid);
  if (!outbeEidStr) throw new Error("Missing required parameter: --outbe-eid");
  const dstEid = Number(outbeEidStr);

  const seriesId = resolveSeriesId(toOptional(args.series));
  const amountStr = toOptional(args.amount);
  if (!amountStr) throw new Error("Missing required parameter: --amount");
  const amount = BigInt(amountStr);

  const { viem } = await (hre as ConnectableHre).network.connect();
  const publicClient = await viem.getPublicClient();
  const wallets = await viem.getWalletClients();
  const wallet = wallets[0];
  if (!wallet) throw new Error("No wallet client available — set the network's PRIVATE_KEY env var");

  const recipient = toOptional(args.recipient) as `0x${string}` | undefined;

  console.log("\n=== Bridge Issued Intex to Outbe (Phase Q2 standalone) ===");
  console.log(`  ONFT1155Adapter:    ${adapterAddress}`);
  console.log(`  Destination EID:    ${dstEid}`);
  console.log(`  Series ID:          ${seriesId}`);
  console.log(`  Amount:             ${amount.toString()}`);
  console.log(`  Recipient:          ${recipient ?? wallet.account.address} (caller)`);

  await bridgeIntexToOutbe(
    { publicClient, walletClient: wallet },
    {
      adapterAddress,
      dstEid,
      seriesId,
      amount,
      recipient,
    },
  );
};

const bridgeToOutbeTask = task(
  "intex-bridge-to-outbe",
  "Phase Q2: bridge Issued Intex from BSC to Outbe via ONFT1155Adapter (requires Qualified state on the source chain).",
)
  .addOption({
    name: "onftAdapterContract",
    description: "ONFT1155Adapter contract on the source chain (required)",
    defaultValue: "",
  })
  .addOption({
    name: "outbeEid",
    description: "Destination LayerZero EID (40812 outbeTestnet, 40712 outbeDevnet, 40512 outbePrivnet)",
    defaultValue: "",
  })
  .addOption({ name: "series", description: "Series in yyyymmdd format (e.g. 20260501)", defaultValue: "" })
  .addOption({ name: "amount", description: "Amount of Issued Intex to bridge (units, not wei)", defaultValue: "" })
  .addOption({
    name: "recipient",
    description: "Override destination recipient (default: caller)",
    defaultValue: "",
  })
  .setAction(lazy(bridgeToOutbeAction));

// =============================================================================
// Export
// =============================================================================

export const qualifiedFlowTasks = [qualifiedFullFlow.build(), bridgeToOutbeTask.build()];
