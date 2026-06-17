// IntexNFT1155 Mark Qualified Task
// Flips a series state from Issued → Qualified on a target IntexNFT1155 instance.
// Caller signer must hold RELAYER_ROLE on the contract.

import { task } from "hardhat/config";
import type { Address } from "viem";

import { markSeriesQualified, type QualifyClient } from "../../scripts/intex/qualify.js";
import { createOutbeClients } from "../../scripts/shared/outbeClient.js";
import { resolveSeriesId } from "../../scripts/shared/auctionId.js";
import { lazy, toOptional } from "../../scripts/shared/taskUtils.js";

// =============================================================================
// Types
// =============================================================================

interface IntexMarkQualifiedTaskArgs {
  intexContract?: string;
  series?: string;
}

interface IntexMarkQualifiedBothTaskArgs {
  intexBscContract?: string;
  intexOutbeContract?: string;
  outbeNetwork?: string;
  series?: string;
}

interface ConnectableHre {
  network: {
    connect: () => Promise<{
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      viem: any;
    }>;
  };
}

// =============================================================================
// Helpers
// =============================================================================

function requireAddress(value: string | undefined, name: string): string {
  if (!value) {
    throw new Error(`Missing required parameter: --${name}`);
  }
  return value;
}

// =============================================================================
// Task Action
// =============================================================================

const intexMarkQualifiedAction = async (args: IntexMarkQualifiedTaskArgs, hre: unknown) => {
  const intexAddress = requireAddress(toOptional(args.intexContract), "intex-contract");
  const seriesId = resolveSeriesId(toOptional(args.series));

  const { viem } = await (hre as ConnectableHre).network.connect();
  const publicClient = await viem.getPublicClient();
  const wallets = await viem.getWalletClients();
  const wallet = wallets[0];
  if (!wallet) {
    throw new Error("No wallet client available — set the network's PRIVATE_KEY env var");
  }

  console.log(`[mark-qualified] target IntexNFT1155: ${intexAddress}`);
  console.log(`[mark-qualified] seriesId:            ${seriesId}`);
  console.log(`[mark-qualified] caller:              ${wallet.account.address}`);

  await markSeriesQualified(
    { publicClient, walletClient: wallet },
    { intexAddress: intexAddress as Address, seriesId },
  );
};

// =============================================================================
// Convenience: mark both BSC and Outbe in one invocation
// =============================================================================
//
// Series state on BSC and Outbe IntexNFT1155 instances is independent — the
// bridge gate on BSC and the settle gate on Outbe both require Qualified, so
// in practice operators want to flip both. This task connects to BSC via the
// hardhat network (so run with `--network bscTestnet` or `bsc`) and switches
// to an Outbe RPC for the second call via `createOutbeClients(outbeNetwork)`.
//
// Each side is wrapped in try/catch — if one chain is already Qualified (or
// fails for another reason), the other still gets attempted.

const intexMarkQualifiedBothAction = async (
  args: IntexMarkQualifiedBothTaskArgs,
  hre: unknown,
) => {
  const intexBsc = requireAddress(toOptional(args.intexBscContract), "intex-bsc-contract");
  const intexOutbe = requireAddress(
    toOptional(args.intexOutbeContract),
    "intex-outbe-contract",
  );
  const outbeNetwork = toOptional(args.outbeNetwork) || "outbeTestnet";
  const seriesId = resolveSeriesId(toOptional(args.series));

  console.log("\n=== Mark Qualified — both chains ===");
  console.log(`  BSC IntexNFT1155:   ${intexBsc}`);
  console.log(`  Outbe IntexNFT1155: ${intexOutbe} (via ${outbeNetwork})`);
  console.log(`  seriesId:           ${seriesId}`);

  // ----- BSC -----
  const { viem } = await (hre as ConnectableHre).network.connect();
  const bscPublic = await viem.getPublicClient();
  const bscWallets = await viem.getWalletClients();
  const bscWallet = bscWallets[0];
  if (!bscWallet) {
    throw new Error("No BSC wallet — set the network's PRIVATE_KEY env var");
  }

  console.log("\n[both] BSC...");
  try {
    await markSeriesQualified(
      { publicClient: bscPublic, walletClient: bscWallet },
      { intexAddress: intexBsc as Address, seriesId },
    );
  } catch (e) {
    console.log(`[both] BSC skipped: ${(e as Error).message}`);
  }

  // ----- Outbe (separate RPC + signer via OUTBE_PRIVATE_KEY) -----
  const outbe = createOutbeClients(outbeNetwork);

  console.log("\n[both] Outbe...");
  try {
    await markSeriesQualified(
      { publicClient: outbe.publicClient, walletClient: outbe.walletClient } as unknown as QualifyClient,
      { intexAddress: intexOutbe as Address, seriesId },
    );
  } catch (e) {
    console.log(`[both] Outbe skipped: ${(e as Error).message}`);
  }

  console.log("\n[both] Done.");
};

// =============================================================================
// Task Definitions
// =============================================================================

const intexMarkQualified = task(
  "intex-mark-qualified",
  "Mark an IntexNFT1155 series as Qualified (Issued → Qualified). Caller must hold RELAYER_ROLE.",
)
  .addOption({
    name: "intexContract",
    description: "IntexNFT1155 contract address (required)",
    defaultValue: "",
  })
  .addOption({
    name: "series",
    description: "Series in yyyymmdd format (e.g. 20260501)",
    defaultValue: "",
  })
  .setAction(lazy(intexMarkQualifiedAction));

const intexMarkQualifiedBoth = task(
  "intex-mark-qualified-both",
  "Mark a series as Qualified on BSC and Outbe IntexNFT1155 in one invocation. Run with --network bscTestnet (or bsc); --outbe-network selects the Outbe RPC.",
)
  .addOption({
    name: "intexBscContract",
    description: "IntexNFT1155 contract address on BSC (required)",
    defaultValue: "",
  })
  .addOption({
    name: "intexOutbeContract",
    description: "IntexNFT1155 contract address on Outbe (required)",
    defaultValue: "",
  })
  .addOption({
    name: "outbeNetwork",
    description: "Outbe network for the second call (outbeTestnet / outbeDevnet / outbePrivnet)",
    defaultValue: "outbeTestnet",
  })
  .addOption({
    name: "series",
    description: "Series in yyyymmdd format (e.g. 20260501)",
    defaultValue: "",
  })
  .setAction(lazy(intexMarkQualifiedBothAction));

// =============================================================================
// Export
// =============================================================================

export const intexQualifyTasks = [intexMarkQualified.build(), intexMarkQualifiedBoth.build()];
