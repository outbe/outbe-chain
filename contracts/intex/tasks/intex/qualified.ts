// Bridge Issued Intex from BSC to Outbe via ONFT1155Adapter (user-driven).
// Run with `--network bscTestnet` (or bsc) — that is the source chain. Requires
// the series to be Qualified on the source chain (the bridge gate).

import { task } from "hardhat/config";

import { bridgeIntexToOutbe } from "../../scripts/intex/bridgeToOutbe.js";
import { resolveSeriesId } from "../../scripts/shared/auctionId.js";
import { lazy, toOptional } from "../../scripts/shared/taskUtils.js";

interface ConnectableHre {
  network: {
    connect: () => Promise<{
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      viem: any;
    }>;
  };
}

function requireAddress(value: string | undefined, name: string): `0x${string}` {
  if (!value) throw new Error(`Missing required parameter: --${name}`);
  return value as `0x${string}`;
}

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

  console.log("\n=== Bridge Issued Intex to Outbe ===");
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
  "Bridge Issued Intex from BSC to Outbe via ONFT1155Adapter (requires Qualified state on the source chain).",
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

export const qualifiedFlowTasks = [bridgeToOutbeTask.build()];
