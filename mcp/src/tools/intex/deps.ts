import { type Address, type Hex, getAddress } from "viem";
import type { Ctx } from "../../chain.js";
import { loadConfig } from "../../config.js";
import { escrowPaymentToken, intexTargets } from "../../intex/reads.js";
import { TARGET_NETWORK } from "../../net/chains.js";
import { ERC20_ABI } from "../../intex/registry.js";
import { type Network, type NetworkResolver, chainIdOf, contextNetwork, networkResolver } from "../../net/resolver.js";
import { receiptSummary, sendCall, waitForReceipt } from "../../net/tx.js";

interface PaymentMeta {
  token: Address;
  decimals: number;
  symbol: string;
}

export interface Submitted {
  txHash: Hex;
  status: string;
  blockNumber?: string;
  gasUsed?: string;
}

/** What every Intex tool section shares: the networks, the signer and the payment-token metadata. */
export interface IntexDeps {
  ctx: Ctx;
  resolveNetwork: NetworkResolver;
  /** The address arg or the configured signer. Throws if neither is available. */
  whoever(explicit?: string): Address;
  /** Submit a tx and, unless wait===false, wait for and summarize its receipt. */
  submit(n: Network, to: Address, data: Hex, value: bigint, wait?: boolean): Promise<Submitted>;
  /** The payment token's address, decimals and symbol, cached per network. */
  paymentMeta(n: Network): Promise<PaymentMeta>;
  /** Resolves `spec` (default: the remote target, bsc-testnet when served) and checks the origin router serves it. */
  target(spec?: string): Promise<Network>;
  /** Where a bridge from `n` lands: `spec`, else outbe, else the one other Intex chain. */
  bridgeDestination(n: Network, spec?: string): Promise<number>;
}

/** Reads a token's metadata once per chain. */
function paymentMetaReader(): (n: Network) => Promise<PaymentMeta> {
  const cache = new Map<number, Promise<PaymentMeta>>();
  const read = async (n: Network): Promise<PaymentMeta> => {
    const token = await escrowPaymentToken(n);
    const [decimals, symbol] = await Promise.all([
      n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "decimals" }),
      n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "symbol" }),
    ]);
    return { token, decimals: Number(decimals), symbol };
  };
  return (n) => {
    const cached = cache.get(n.chainId) ?? read(n).catch((error) => {
      cache.delete(n.chainId);
      throw error;
    });
    cache.set(n.chainId, cached);
    return cached;
  };
}

/** Every chain running Intex: the connected Outbe node and the origin router's targets, read once. */
function intexChainsReader(ctx: Ctx): () => Promise<number[]> {
  let targets: Promise<number[]> | undefined;
  return async () => {
    targets ??= intexTargets(contextNetwork(ctx)).catch((error) => {
      targets = undefined;
      throw error;
    });
    return [...new Set([ctx.chain.id, ...(await targets)])];
  };
}

/** The remote target Intex tools read when no network is named. */
function defaultTargetOf(ctx: Ctx, chainIds: number[]): number {
  const remote = chainIds.filter((id) => id !== ctx.chain.id);
  const preferred = chainIdOf(TARGET_NETWORK, ctx);
  if (remote.length === 0) return ctx.chain.id;
  if (remote.length === 1) return remote[0];
  if (remote.includes(preferred)) return preferred;
  throw new Error(`pass network: the origin router serves chains ${remote.join(", ")}`);
}

function bridgeDestinationOf(ctx: Ctx, n: Network, peers: number[], spec?: string): number {
  let destination: number | undefined = ctx.chain.id;
  if (spec !== undefined) destination = chainIdOf(spec, ctx);
  else if (n.isOutbe) destination = peers.length === 1 ? peers[0] : undefined;
  if (destination === undefined) {
    throw new Error(`pass destination: ${n.name} bridges to chains ${peers.join(", ")}`);
  }
  if (!peers.includes(destination)) {
    throw new Error(`${n.name} cannot bridge to chain ${destination}; its Intex peers are ${peers.join(", ")}`);
  }
  return destination;
}

export function intexDeps(ctx: Ctx): IntexDeps {
  const resolveNetwork = networkResolver(ctx, loadConfig());
  const intexChains = intexChainsReader(ctx);
  return {
    ctx,
    resolveNetwork,
    whoever(explicit) {
      if (explicit) return getAddress(explicit);
      if (ctx.account) return ctx.account.address;
      throw new Error("no address given and no signer configured - pass an explicit address");
    },
    async submit(n, to, data, value, wait) {
      const hash = await sendCall(ctx, n, { to, data, value });
      if (wait === false) return { txHash: hash, status: "submitted" as const };
      return { txHash: hash, ...receiptSummary(await waitForReceipt(n, hash)) };
    },
    paymentMeta: paymentMetaReader(),
    async target(spec) {
      const chainIds = await intexChains();
      const n = await resolveNetwork(spec ?? String(defaultTargetOf(ctx, chainIds)));
      if (!chainIds.includes(n.chainId)) {
        throw new Error(`${n.name} is not an Intex target; the origin router serves chains ${chainIds.join(", ")}`);
      }
      return n;
    },
    async bridgeDestination(n, spec) {
      const peers = (await intexChains()).filter((id) => id !== n.chainId);
      return bridgeDestinationOf(ctx, n, peers, spec);
    },
  };
}
