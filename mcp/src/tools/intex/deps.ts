import { type Address, type Hex, getAddress } from "viem";
import type { Ctx } from "../../chain.js";
import { loadConfig } from "../../config.js";
import { addr } from "../../intex/reads.js";
import { ERC20_ABI } from "../../intex/registry.js";
import { type Network, type NetworkResolver, networkResolver } from "../../net/resolver.js";
import { receiptSummary, sendCall, waitForReceipt } from "../../net/tx.js";

interface PaymentMeta {
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
  /** The payment token's decimals and symbol, cached per network. */
  paymentMeta(n: Network): Promise<PaymentMeta>;
}

export function intexDeps(ctx: Ctx): IntexDeps {
  const metaCache = new Map<string, PaymentMeta>();
  return {
    ctx,
    resolveNetwork: networkResolver(ctx, loadConfig()),
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
      const cached = metaCache.get(n.name);
      if (cached) return cached;
      const token = addr(n, "paymentToken");
      const [decimals, symbol] = (await Promise.all([
        n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "decimals" }),
        n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "symbol" }),
      ])) as [number, string];
      const meta = { decimals: Number(decimals), symbol };
      metaCache.set(n.name, meta);
      return meta;
    },
  };
}
