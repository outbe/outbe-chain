// Bridge Issued Intex from BSC to Outbe via ONFT1155Adapter (user-driven).
// Run with `--network bscTestnet` (or bsc) — that is the source chain. Requires
// the series to be Qualified on the source chain (the bridge gate).

import { task } from "hardhat/config";
import { createPublicClient, createWalletClient, http } from "viem";
import { privateKeyToAccount } from "viem/accounts";

import { bridgeIntexToOutbe } from "../../scripts/intex/bridgeToOutbe.js";
import { resolveSeriesId } from "../../scripts/shared/auctionId.js";
import { getEnvRpcAndPk, makeChain } from "../../scripts/shared/layerzero.js";
import { getNetworkName, lazy, toOptional } from "../../scripts/shared/taskUtils.js";

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

  const networkName = getNetworkName(hre);
  const { rpc, pk } = getEnvRpcAndPk(networkName);
  if (!pk) throw new Error(`Private key required for ${networkName}`);
  const chain = makeChain(networkName, rpc);
  const account = privateKeyToAccount(pk as `0x${string}`);
  const transport = http(rpc);
  const publicClient = createPublicClient({ chain, transport });
  const walletClient = createWalletClient({ account, chain, transport });

  const recipient = toOptional(args.recipient) as `0x${string}` | undefined;

  console.log("\n=== Bridge Issued Intex to Outbe ===");
  console.log(`  ONFT1155Adapter:    ${adapterAddress}`);
  console.log(`  Destination EID:    ${dstEid}`);
  console.log(`  Series ID:          ${seriesId}`);
  console.log(`  Amount:             ${amount.toString()}`);
  console.log(`  Recipient:          ${recipient ?? account.address} (caller)`);

  await bridgeIntexToOutbe(
    { publicClient, walletClient },
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
