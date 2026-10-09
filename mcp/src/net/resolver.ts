import type { Chain, PublicClient, WalletClient } from "viem";
import { type Ctx, createCtx } from "../chain.js";
import { NETWORKS } from "./chains.js";

/** A chain the tools read and sign on: the connected `ctx` or a fresh client. */
export interface Network {
  name: string;
  chainId: number;
  chain: Chain;
  client: PublicClient;
  wallet?: WalletClient;
}

export type NetworkResolver = (spec: string) => Promise<Network>;

/** Resolves a NETWORKS entry by name or chain id, reusing `ctx` when the chain id matches. */
export function networkResolver(ctx: Ctx, privateKey?: string): NetworkResolver {
  const cache = new Map<string, Network>();
  return async (spec) => {
    const s = spec.trim().toLowerCase();
    const def = NETWORKS.find((d) => d.name.toLowerCase() === s || String(d.chainId) === s);
    if (!def) {
      throw new Error(`unknown network "${spec}"; supported: ${NETWORKS.map((d) => d.name).join(", ")}`);
    }
    const cached = cache.get(def.name);
    if (cached) return cached;
    const c = def.chainId === ctx.chain.id ? ctx : await createCtx(def.rpc, privateKey);
    const network: Network = {
      name: def.name,
      chainId: c.chain.id,
      chain: c.chain,
      client: c.publicClient,
      wallet: c.walletClient,
    };
    cache.set(def.name, network);
    return network;
  };
}
