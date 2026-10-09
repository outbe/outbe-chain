import type { Chain, PublicClient, WalletClient } from "viem";
import { type Ctx, createCtx } from "../chain.js";
import { KNOWN_CHAINS, NETWORK_NAMES, OUTBE_NETWORK, chainName } from "./chains.js";

/** A chain the tools read and sign on: the connected Outbe node or another chain by its RPC. */
export interface Network {
  name: string;
  chainId: number;
  chain: Chain;
  client: PublicClient;
  wallet?: WalletClient;
  /** Whether this is the connected Outbe node. */
  isOutbe: boolean;
}

export type NetworkResolver = (spec: string) => Promise<Network>;

export interface NetworkSettings {
  privateKey?: string;
  /** RPC URLs by chain id, from `OUTBE_RPC_<chainId>`. */
  chainRpcs: Record<number, string>;
}

function network(ctx: Ctx, isOutbe: boolean): Network {
  const id = ctx.chain.id;
  return {
    name: chainName(id) ?? `${isOutbe ? "outbe" : "chain"}-${id}`,
    chainId: id,
    chain: ctx.chain,
    client: ctx.publicClient,
    wallet: ctx.walletClient,
    isOutbe,
  };
}

/** The connected outbe node as a `Network`. */
export function contextNetwork(ctx: Ctx): Network {
  return network(ctx, true);
}

function chainIdOf(spec: string, ctx: Ctx): number {
  const s = spec.trim().toLowerCase();
  if (s === OUTBE_NETWORK) return ctx.chain.id;
  const known = KNOWN_CHAINS.find((c) => c.name === s);
  if (known) return known.chainId;
  if (/^\d+$/.test(s)) return Number(s);
  throw new Error(`unknown network "${spec}"; use ${NETWORK_NAMES.join(", ")} or a chain id`);
}

async function remoteNetwork(chainId: number, settings: NetworkSettings): Promise<Network> {
  const rpc = settings.chainRpcs[chainId] ?? KNOWN_CHAINS.find((c) => c.chainId === chainId)?.rpc;
  if (!rpc) throw new Error(`no RPC for chain ${chainId}; set OUTBE_RPC_${chainId}`);
  const c = await createCtx(rpc, settings.privateKey);
  if (c.chain.id !== chainId) throw new Error(`the RPC for chain ${chainId} serves chain ${c.chain.id}`);
  return network(c, false);
}

/** Resolves `outbe`, a known chain name or a chain id; the connected node's chain id is always Outbe. */
export function networkResolver(ctx: Ctx, settings: NetworkSettings): NetworkResolver {
  const cache = new Map<number, Network>();
  return async (spec) => {
    const chainId = chainIdOf(spec, ctx);
    const cached = cache.get(chainId);
    if (cached) return cached;
    const resolved = chainId === ctx.chain.id ? contextNetwork(ctx) : await remoteNetwork(chainId, settings);
    cache.set(chainId, resolved);
    return resolved;
  };
}
