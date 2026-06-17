// Outbe-network viem client factory.
// Builds public/wallet clients for an Outbe chain from OUTBE_CHAINS, signing with
// OUTBE_PRIVATE_KEY (RPC overridable via OUTBE_RPC_URL).

import { http, createPublicClient, createWalletClient } from "viem";
import { privateKeyToAccount } from "viem/accounts";

import { OUTBE_CHAINS } from "./layerzero.js";

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
