import { type Address, getAddress } from "viem";
import { type Ctx, readView } from "../../chain.js";
import { ensureAllowance } from "../../net/erc20.js";
import { contextNetwork } from "../../net/resolver.js";
import { resolveContract } from "../../registry.js";
import { submit } from "./submit.js";

/** A right's settlement quote for paying with `asset`. */
export async function quoteRightSettlement(ctx: Ctx, factory: string, id: bigint, asset: Address) {
  const { result } = await readView(ctx, resolveContract(factory), "quoteSettlement", [id, getAddress(asset)]);
  const [settlementCurrency, paymentMinor, snapshotId] = result as [number, bigint, bigint];
  return { settlementCurrency, paymentMinor, snapshotId };
}

/** A right to settle: its factory, that factory's settle method, and the right's id. */
export interface Right {
  factory: string;
  method: string;
  id: bigint;
}

/** Quotes a right, approves its factory for the cost if the allowance is short, and settles at the quoted snapshot. */
export async function settleRight(ctx: Ctx, { factory, method, id }: Right, asset: Address, wait?: boolean) {
  const token = getAddress(asset);
  const spender = resolveContract(factory).address;
  const { settlementCurrency, paymentMinor, snapshotId } = await quoteRightSettlement(ctx, factory, id, token);
  const approval =
    paymentMinor > 0n ? await ensureAllowance(ctx, contextNetwork(ctx), { token, spender, amount: paymentMinor }) : null;
  return submit(ctx, {
    contract: factory,
    method,
    args: [id, token, snapshotId],
    wait,
    extra: {
      settlementCurrency,
      paymentMinor: paymentMinor.toString(),
      snapshotId: snapshotId.toString(),
      autoApprove: approval ? { txHash: approval, amount: paymentMinor.toString() } : null,
    },
  });
}
