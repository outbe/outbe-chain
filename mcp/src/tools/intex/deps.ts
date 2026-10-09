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
  /** Resolves `spec` (default the target network) and checks the origin router fans out to it. */
  target(spec?: string): Promise<Network>;
  /** Where a bridge from `n` lands: `spec`, else outbe, else the one other Intex chain. */
  bridgeDestination(n: Network, spec?: string): Promise<number>;
}

export function intexDeps(ctx: Ctx): IntexDeps {
  const metaCache = new Map<number, PaymentMeta>();
  const resolveNetwork = networkResolver(ctx, loadConfig());
  let targets: Promise<number[]> | undefined;
  const intexChains = async (): Promise<number[]> => {
    targets ??= intexTargets(contextNetwork(ctx));
    const chainIds = await targets.catch((error) => {
      targets = undefined;
      throw error;
    });
    return [...new Set([ctx.chain.id, ...chainIds])];
  };
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
    async paymentMeta(n) {
      const cached = metaCache.get(n.chainId);
      if (cached) return cached;
      const token = await escrowPaymentToken(n);
      const [decimals, symbol] = (await Promise.all([
        n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "decimals" }),
        n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "symbol" }),
      ])) as [number, string];
      const meta = { token, decimals: Number(decimals), symbol };
      metaCache.set(n.chainId, meta);
      return meta;
    },
    async target(spec) {
      const n = await resolveNetwork(spec ?? TARGET_NETWORK);
      const chainIds = await intexChains();
      if (!chainIds.includes(n.chainId)) {
        throw new Error(`${n.name} is not an Intex target; the origin router serves chains ${chainIds.join(", ")}`);
      }
      return n;
    },
    async bridgeDestination(n, spec) {
      const chainIds = (await intexChains()).filter((id) => id !== n.chainId);
      const destination =
        spec !== undefined ? chainIdOf(spec, ctx) : n.isOutbe ? (chainIds.length === 1 ? chainIds[0] : undefined) : ctx.chain.id;
      if (destination === undefined) {
        throw new Error(`pass destination: ${n.name} bridges to chains ${chainIds.join(", ")}`);
      }
      if (!chainIds.includes(destination)) {
        throw new Error(`${n.name} cannot bridge to chain ${destination}; its Intex peers are ${chainIds.join(", ")}`);
      }
      return destination;
    },
  };
}
