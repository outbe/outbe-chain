// Settlement Tasks
// Phase 1 — System Bridge: markCalled → bridge all Intex holders from BSC to Outbe.
// Phase 2 — Settle on Outbe: approve stablecoins → settle() → burn Issued → mint Settled.
// Phase 3 — Mine Promis on Outbe: holder calls IntexSettlement.minePromis(seriesId, amount)
//           to atomically burn their Settled balance and receive amount*promisLoadMinor Promis.
//
// Pre-requisite: auction flow completed, Intex issued on BSC.
//
// Tasks:
//   settlement-interactive   — Phase 1 interactive flow (bridge holders)
//   settlement-full-flow     — Phase 1 + Phase 2 + Phase 3 combined
//   settlement-settle        — Phase 2 only (approve + settle)
//   settlement-settle-preview — Phase 2 preview (read-only)
//   settlement-mine          — Phase 3 only (mine Promis from Settled balance)
//   settlement-check         — Check series status on both chains
//   settlement-mark-called   — Send markSeriesCalled to BSC
//   settlement-fund-adapter  — Fund TargetMessenger on BSC

import { task } from "hardhat/config";
import {
  createSettlementRuntime,
  createSeriesOnOutbe,
  fundBridgeAdapter,
  markSeriesCalled,
  waitForMarkCalledDelivery,
  waitForSystemBridgeDelivery,
  verifyMigration,
  checkSeriesStatus,
  getBscHolderCount,
  getOutbeSeriesState,
} from "../../scripts/intex/settlementBridge.js";
import { resolveSeriesId } from "../../scripts/shared/auctionId.js";
import { promptForContinue } from "../../scripts/shared/prompt.js";
import { executeSettle, previewSettle, createOutbeClients } from "../../scripts/intex/settle.js";
import { minePromis, type MineClient } from "../../scripts/intex/mine.js";
import { lazy, toOptional, parseBoolean, getNetworkName } from "../../scripts/shared/taskUtils.js";
import type { SettlementInteractiveTaskArgs, SettlementSettleTaskArgs, SettlementFullFlowTaskArgs } from "../types.js";

// =============================================================================
// Helpers
// =============================================================================

function requireAddress(value: string | undefined, name: string): `0x${string}` {
  if (!value) throw new Error(`Missing required parameter: --${name}`);
  return value as `0x${string}`;
}

function toOptionalBigInt(input: string | undefined): bigint | undefined {
  if (!input) return undefined;
  const trimmed = input.trim();
  return trimmed === "" ? undefined : BigInt(trimmed);
}

// =============================================================================
// Interactive Flow (Phase 1: System Bridge)
// =============================================================================

const settlementInteractiveAction = async (args: SettlementInteractiveTaskArgs, hre: unknown) => {
  const desisContract = requireAddress(toOptional(args.desisContract), "telosis-contract");
  const intexBscContract = requireAddress(toOptional(args.intexBscContract), "intex-bsc-contract");
  const intexOutbeContract = requireAddress(toOptional(args.intexOutbeContract), "intex-outbe-contract");
  const targetMessengerContract = requireAddress(toOptional(args.targetMessengerContract), "bnb-bridge-adapter-contract");

  const bscNetwork = (toOptional(args.bscNetwork) || "bscTestnet") as string;
  const skipFund = parseBoolean(args.skipFund, false);

  const seriesId = resolveSeriesId(toOptional(args.series));
  const msgValue = toOptionalBigInt(toOptional(args.msgValue));

  const runtime = await createSettlementRuntime(hre, {
    telosisAddress: desisContract,
    intexOutbeAddress: intexOutbeContract,
  });

  console.log("\n=== Settlement Interactive Flow (Phase 1: System Bridge) ===");
  console.log({
    desisContract,
    intexBscContract,
    intexOutbeContract,
    targetMessengerContract,
    seriesId,
    bscNetwork,
  });

  // Step 1: Check current status
  console.log("\n[1/7] Checking series status...");
  await checkSeriesStatus(runtime, {
    seriesId,
    intexBscAddress: intexBscContract,
    intexOutbeAddress: intexOutbeContract,
    bscNetworkId: bscNetwork,
  });

  const bscHolderCount = await getBscHolderCount(intexBscContract, seriesId, bscNetwork);
  if (bscHolderCount === 0) {
    console.log("\n[!] No holders on BSC — nothing to bridge. Exiting.");
    return;
  }
  console.log(`\n   BSC holders to migrate: ${bscHolderCount}`);

  // Step 2: Create series on Outbe IntexNFT1155 (read params from BSC)
  // NOTE: In the standard flow, Telosis creates the series during sendIssuanceInstructions.
  //       This step is a fallback for cases where the series was not created during the auction flow.
  console.log("\n[2/7] Creating series on Outbe IntexNFT1155 (fallback if not created during auction)...");
  await createSeriesOnOutbe(runtime, {
    seriesId,
    intexBscAddress: intexBscContract,
    intexOutbeAddress: intexOutbeContract,
    bscNetworkId: bscNetwork,
  });

  // Step 3: Fund TargetMessenger
  if (!skipFund) {
    console.log("\n[3/7] Funding TargetMessenger on BSC for system bridge LZ fees...");
    await fundBridgeAdapter({
      bridgeAdapterAddress: targetMessengerContract,
      networkId: bscNetwork,
    });
  } else {
    console.log("\n[3/7] Skipping adapter funding (--skip-fund)");
  }

  // Check IntexNFT1155 state on Outbe — if already Called, skip markSeriesCalled.
  // Series state ordinals: Issued=0, Qualified=1, Called=2.
  const outbeState = await getOutbeSeriesState(runtime, seriesId);
  const INTEX_STATE_CALLED = 2;
  if (outbeState != null && outbeState >= INTEX_STATE_CALLED) {
    console.log("\n[4/7] Series already Called on Outbe — skipping markSeriesCalled");
    console.log("[5/7] Skipping LZ wait (markSeriesCalled already delivered)");
  } else {
    await promptForContinue("Press Enter to trigger markSeriesCalled...");

    // Step 4: markSeriesCalled via Telosis
    console.log("\n[4/7] Calling markSeriesCalled on Telosis...");
    await markSeriesCalled(runtime, { seriesId, msgValue });

    // Step 5: Wait for MSG_MARK_CALLED delivery on BSC
    console.log("\n[5/7] Waiting for MSG_MARK_CALLED delivery on BSC...");
    const bridgeAdapterAddress = await runtime.telosis.read.bridgeAdapter();
    const outbeNetworkId = getNetworkName(hre, "outbeDevnet");
    await waitForMarkCalledDelivery(runtime, {
      originMessengerAddress: bridgeAdapterAddress,
      bscNetworkId: bscNetwork,
      outbeNetworkId,
    });
  }

  // Step 6: Wait for system bridge delivery on Outbe
  console.log("\n[6/7] Waiting for system bridge (SEND_MULTI) delivery on Outbe...");
  let bridgeTimeout = false;
  try {
    await waitForSystemBridgeDelivery(runtime, {
      intexOutbeAddress: intexOutbeContract,
      seriesId,
      expectedHolderCount: bscHolderCount,
    });
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    if (msg.includes("Timeout")) {
      bridgeTimeout = true;
      console.log("[6/7] System bridge delivery timed out - continuing to verify...");
    } else {
      throw e;
    }
  }

  // Step 7: Verify migration
  console.log("\n[7/7] Verifying migration...");
  const result = await verifyMigration(runtime, {
    seriesId,
    intexBscAddress: intexBscContract,
    intexOutbeAddress: intexOutbeContract,
    bscNetworkId: bscNetwork,
  });

  // Determine status based on migration outcome
  const bscRemaining = result.bscHolders.length;
  const outbeMigrated = result.outbeHolders.length;

  let status: "SUCCESS" | "PARTIAL" | "FAIL" | "TIMEOUT";
  if (bscRemaining === 0 && outbeMigrated > 0) {
    status = "SUCCESS";
  } else if (bscRemaining === 0 && outbeMigrated === 0) {
    status = bridgeTimeout ? "TIMEOUT" : "FAIL";
  } else if (bscRemaining > 0 && outbeMigrated > 0) {
    status = "PARTIAL";
  } else {
    status = bridgeTimeout ? "TIMEOUT" : "FAIL";
  }

  console.log("\n=== Phase 1 Complete ===");
  console.log({
    seriesId,
    bscHoldersRemaining: bscRemaining,
    outbeHoldersMigrated: outbeMigrated,
    status,
  });

  if (status === "TIMEOUT") {
    console.log("\n[!] MIGRATION TIMEOUT: system bridge delivery did not complete in time.");
    console.log("    LayerZero message may still be in flight. Check:");
    console.log("    - https://layerzeroscan.com for message status");
    console.log("    - TargetMessenger balance on BSC for sufficient gas");
    console.log("    Re-run settlement-check later to verify final state.");
  } else if (status === "FAIL") {
    console.log("\n[!] MIGRATION FAILED: tokens burned on BSC but not received on Outbe.");
    console.log("    Check LayerZero message delivery and TargetMessenger balance.");
  } else if (status === "PARTIAL") {
    console.log("\n[!] MIGRATION PARTIAL: some holders migrated, some remain on BSC.");
  } else if (outbeMigrated > 0) {
    console.log("\n=== Phase 2: Settlement ===");
    console.log("   Holders now have Intex on Outbe and can settle via IntexSettlement contract.");
    console.log("   Run: yarn hardhat settlement-settle --network <outbeNetwork> --settlement-contract <addr> --intex-contract <addr> --auction-id <id>");
  }
};

// =============================================================================
// Full Flow: Phase 1 (bridge) + Phase 2 (settle)
// =============================================================================

const settlementFullFlowAction = async (args: SettlementFullFlowTaskArgs, hre: unknown) => {
  const settlementContract = requireAddress(toOptional(args.settlementContract), "settlement-contract");

  // --- Phase 1: System Bridge ---
  await settlementInteractiveAction(args, hre);

  // --- Phase 2: Settle from caller's wallet (full balance) ---
  const seriesId = resolveSeriesId(toOptional(args.series));
  const intexOutbeContract = requireAddress(toOptional(args.intexOutbeContract), "intex-outbe-contract");
  const intexHolder = toOptional(args.intexHolder) as `0x${string}` | undefined;
  const promisContract = toOptional(args.promisContract) as `0x${string}` | undefined;

  await promptForContinue("\nPhase 1 complete. Press Enter to proceed to Phase 2 (settle)...");

  console.log("\n=== Phase 2: Settlement (full balance) ===");
  await executeSettle(hre, {
    settlementAddress: settlementContract,
    intexAddress: intexOutbeContract,
    promisAddress: promisContract,
    seriesId,
    intexHolder,
  });

  // Phase 3 only runs when a promis-contract was provided (it's optional in Phase 2 args).
  if (promisContract) {
    await promptForContinue("\nPhase 2 complete. Press Enter to proceed to Phase 3 (mine Promis)...");

    console.log("\n=== Phase 3: Mine Promis ===");
    await executeMine(hre, {
      settlementAddress: settlementContract,
      promisAddress: promisContract,
      intexAddress: intexOutbeContract,
      seriesId,
    });
  } else {
    console.log(
      "\nPhase 3 skipped — `--promis-contract` was not provided. Run `settlement-mine` separately when ready.",
    );
  }
};

// =============================================================================
// Individual Step Tasks
// =============================================================================

const checkStatusAction = async (args: SettlementInteractiveTaskArgs, hre: unknown) => {
  const desisContract = requireAddress(toOptional(args.desisContract), "telosis-contract");
  const intexBscContract = toOptional(args.intexBscContract) as `0x${string}` | undefined;
  const intexOutbeContract = toOptional(args.intexOutbeContract) as `0x${string}` | undefined;
  const bscNetwork = toOptional(args.bscNetwork) || "bscTestnet";

  const seriesId = resolveSeriesId(toOptional(args.series));

  const runtime = await createSettlementRuntime(hre, {
    telosisAddress: desisContract,
    intexOutbeAddress: intexOutbeContract,
  });

  await checkSeriesStatus(runtime, {
    seriesId,
    intexBscAddress: intexBscContract,
    intexOutbeAddress: intexOutbeContract,
    bscNetworkId: bscNetwork,
  });
};

const markCalledAction = async (args: SettlementInteractiveTaskArgs, hre: unknown) => {
  const desisContract = requireAddress(toOptional(args.desisContract), "telosis-contract");

  const seriesId = resolveSeriesId(toOptional(args.series));
  const msgValue = toOptionalBigInt(toOptional(args.msgValue));

  const runtime = await createSettlementRuntime(hre, {
    telosisAddress: desisContract,
  });

  await markSeriesCalled(runtime, { seriesId, msgValue });
  console.log("markSeriesCalled sent for:", seriesId);
};

const fundAdapterAction = async (args: SettlementInteractiveTaskArgs, _hre: unknown) => {
  const adapterAddress = requireAddress(toOptional(args.targetMessengerContract), "bnb-bridge-adapter-contract");
  const bscNetwork = toOptional(args.bscNetwork) || "bscTestnet";

  await fundBridgeAdapter({ bridgeAdapterAddress: adapterAddress, networkId: bscNetwork });
};

// =============================================================================
// Phase 2: Settle (approve + settle on Outbe)
// =============================================================================

const settleAction = async (args: SettlementSettleTaskArgs, hre: unknown) => {
  const settlementContract = requireAddress(toOptional(args.settlementContract), "settlement-contract");
  const intexContract = requireAddress(toOptional(args.intexContract), "intex-contract");
  const promisContract = toOptional(args.promisContract) as `0x${string}` | undefined;

  const seriesId = resolveSeriesId(toOptional(args.series));
  const intexHolder = toOptional(args.intexHolder) as `0x${string}` | undefined;

  console.log(`\n=== Settlement Phase 2: Settle (full balance) ===`);
  console.log(`  Series ID: ${seriesId}`);
  console.log(`  Settlement: ${settlementContract}`);
  console.log(`  IntexNFT1155: ${intexContract}`);
  if (promisContract) console.log(`  Promis: ${promisContract}`);
  if (intexHolder) console.log(`  IntexHolder: ${intexHolder}`);

  await executeSettle(hre, {
    settlementAddress: settlementContract,
    intexAddress: intexContract,
    promisAddress: promisContract,
    seriesId,
    intexHolder,
  });
};

const settlePreviewAction = async (args: SettlementSettleTaskArgs, hre: unknown) => {
  const settlementContract = requireAddress(toOptional(args.settlementContract), "settlement-contract");
  const intexContract = requireAddress(toOptional(args.intexContract), "intex-contract");

  const seriesId = resolveSeriesId(toOptional(args.series));
  const intexHolder = toOptional(args.intexHolder) as `0x${string}` | undefined;

  const { publicClient, account } = createOutbeClients(getNetworkName(hre));

  const preview = await previewSettle(publicClient, account.address, {
    settlementAddress: settlementContract,
    intexAddress: intexContract,
    seriesId,
    intexHolder,
  });

  const { formatUnits } = await import("viem");

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
  console.log(`  Needs approval:      ${preview.needsApproval}`);
  console.log(`  Promis (post-mine):  ${preview.promisToMint}`);
};

// =============================================================================
// Phase 3: Mine Promis from Settled Intex
// =============================================================================

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

interface MineOpts {
  settlementAddress: `0x${string}`;
  promisAddress: `0x${string}`;
  intexAddress: `0x${string}`;
  seriesId: number;
  /** If undefined, the caller's full Settled balance is mined. */
  amount?: bigint;
}

async function executeMine(hre: unknown, opts: MineOpts) {
  const networkName = getNetworkName(hre);
  const { publicClient, walletClient } = createOutbeClients(networkName);
  const holder = walletClient.account.address;

  const settledId = await publicClient.readContract({
    address: opts.intexAddress,
    abi: SETTLED_TOKEN_READ_ABI,
    functionName: "settledTokenId",
    args: [opts.seriesId],
  });
  const settledBalance = await publicClient.readContract({
    address: opts.intexAddress,
    abi: SETTLED_TOKEN_READ_ABI,
    functionName: "balanceOf",
    args: [holder, settledId],
  });

  const amount = opts.amount ?? settledBalance;
  if (amount === 0n) {
    console.log(`[mine] Nothing to mine — Settled balance for ${holder} is 0. Skipping.`);
    return;
  }

  await minePromis(
    { publicClient, walletClient } as unknown as MineClient,
    {
      settlementAddress: opts.settlementAddress,
      promisAddress: opts.promisAddress,
      intexAddress: opts.intexAddress,
      seriesId: opts.seriesId,
      amount,
    },
  );
}

const mineAction = async (
  args: SettlementSettleTaskArgs & { amount?: string },
  hre: unknown,
) => {
  const settlementContract = requireAddress(toOptional(args.settlementContract), "settlement-contract");
  const promisContract = requireAddress(toOptional(args.promisContract), "promis-contract");
  const intexContract = requireAddress(toOptional(args.intexContract), "intex-contract");

  const seriesId = resolveSeriesId(toOptional(args.series));
  const amount = toOptionalBigInt(toOptional(args.amount));

  console.log(`\n=== Settlement Phase 3: Mine Promis ===`);
  console.log(`  Series ID:  ${seriesId}`);
  console.log(`  Settlement: ${settlementContract}`);
  console.log(`  Promis:     ${promisContract}`);
  console.log(`  Intex:      ${intexContract}`);
  console.log(`  Amount:     ${amount === undefined ? "(full Settled balance)" : amount.toString()}`);

  await executeMine(hre, {
    settlementAddress: settlementContract,
    promisAddress: promisContract,
    intexAddress: intexContract,
    seriesId,
    amount,
  });
};

// =============================================================================
// Task Definitions
// =============================================================================

const COMMON_OPTIONS = [
  { name: "desisContract", description: "Telosis contract address (Outbe)", defaultValue: "" },
  { name: "intexBscContract", description: "IntexNFT1155 contract address (BSC)", defaultValue: "" },
  { name: "intexOutbeContract", description: "IntexNFT1155 contract address (Outbe)", defaultValue: "" },
  { name: "targetMessengerContract", description: "TargetMessenger contract address (BSC)", defaultValue: "" },
  { name: "series", description: "Series in yyyymmdd format", defaultValue: "" },
  { name: "bscNetwork", description: "BSC network (bscTestnet or bsc)", defaultValue: "bscTestnet" },
] as const;

function addCommonOptions(t: ReturnType<typeof task>) {
  let builder = t;
  for (const opt of COMMON_OPTIONS) {
    builder = builder.addOption(opt);
  }
  return builder;
}

const settlementInteractive = addCommonOptions(
  task("settlement-interactive", "Interactive settlement flow: Phase 1 system bridge"),
)
  .addOption({ name: "msgValue", description: "Native token value for LZ fees (wei)", defaultValue: "" })
  .addOption({ name: "skipFund", description: "Skip TargetMessenger funding", defaultValue: "false" })
  .setAction(lazy(settlementInteractiveAction));

const settlementCheckStatus = addCommonOptions(
  task("settlement-check", "Check settlement series status on both chains"),
).setAction(lazy(checkStatusAction));

const settlementMarkCalled = addCommonOptions(
  task("settlement-mark-called", "Send markSeriesCalled to BSC"),
)
  .addOption({ name: "msgValue", description: "Native token value for LZ fees (wei)", defaultValue: "" })
  .setAction(lazy(markCalledAction));

const settlementFundAdapter = addCommonOptions(
  task("settlement-fund-adapter", "Fund TargetMessenger on BSC for system bridge LZ fees"),
).setAction(lazy(fundAdapterAction));

const SETTLE_OPTIONS = [
  { name: "settlementContract", description: "IntexSettlement contract address (Outbe)", defaultValue: "" },
  { name: "intexContract", description: "IntexNFT1155 contract address (Outbe)", defaultValue: "" },
  { name: "promisContract", description: "Promis contract address (Outbe, optional — for balance check)", defaultValue: "" },
  { name: "series", description: "Series in yyyymmdd format", defaultValue: "" },
  { name: "intexHolder", description: "Intex holder address (default: payer/caller)", defaultValue: "" },
] as const;

function addSettleOptions(t: ReturnType<typeof task>) {
  let builder = t;
  for (const opt of SETTLE_OPTIONS) {
    builder = builder.addOption(opt);
  }
  return builder;
}

const settlementFullFlow = addCommonOptions(
  task(
    "settlement-full-flow",
    "Full flow: Phase 1 system bridge + Phase 2 settle + Phase 3 mine (Phase 3 runs when --promis-contract is supplied)",
  ),
)
  .addOption({ name: "msgValue", description: "Native token value for LZ fees (wei)", defaultValue: "" })
  .addOption({ name: "skipFund", description: "Skip TargetMessenger funding", defaultValue: "false" })
  .addOption({ name: "settlementContract", description: "IntexSettlement contract address (Outbe, required for Phase 2)", defaultValue: "" })
  .addOption({ name: "promisContract", description: "Promis contract address (Outbe). When set, Phase 3 (mine) runs after Phase 2.", defaultValue: "" })
  .addOption({ name: "intexHolder", description: "Intex holder address (default: payer/caller)", defaultValue: "" })
  .setAction(lazy(settlementFullFlowAction));

const settlementSettle = addSettleOptions(
  task(
    "settlement-settle",
    "Phase 2: approve payment token + settle full balance (burn Issued, mint Settled). Mining Promis is a separate Phase 3 step — see `settlement-mine`.",
  ),
).setAction(lazy(settleAction));

const settlementSettlePreview = addSettleOptions(
  task("settlement-settle-preview", "Preview settlement: show cost, balances, allowance (read-only)"),
).setAction(lazy(settlePreviewAction));

const MINE_OPTIONS = [
  { name: "settlementContract", description: "IntexSettlement contract address (Outbe, required)", defaultValue: "" },
  { name: "promisContract", description: "Promis contract address (Outbe, required for balance display)", defaultValue: "" },
  { name: "intexContract", description: "IntexNFT1155 contract address (Outbe, required)", defaultValue: "" },
  { name: "series", description: "Series in yyyymmdd format", defaultValue: "" },
  {
    name: "amount",
    description: "Number of Settled Intex to mine. Defaults to the caller's full Settled balance for the series.",
    defaultValue: "",
  },
] as const;

function addMineOptions(t: ReturnType<typeof task>) {
  let builder = t;
  for (const opt of MINE_OPTIONS) {
    builder = builder.addOption(opt);
  }
  return builder;
}

const settlementMine = addMineOptions(
  task(
    "settlement-mine",
    "Phase 3: mine Promis through IntexSettlement.minePromis — burns Settled Intex and mints amount*promisLoadMinor Promis to the caller atomically.",
  ),
).setAction(lazy(mineAction));

// =============================================================================
// Export
// =============================================================================

export const settlementTasks = [
  settlementInteractive.build(),
  settlementFullFlow.build(),
  settlementCheckStatus.build(),
  settlementMarkCalled.build(),
  settlementFundAdapter.build(),
  settlementSettle.build(),
  settlementSettlePreview.build(),
  settlementMine.build(),
];
