import { type Ctx, sendTx } from "../../chain.js";
import { resolveContract } from "../../registry.js";
import { ok } from "../util.js";

export const GAS_DEFAULT = 3_000_000n;

export interface Write {
  contract: string;
  method: string;
  args: unknown[];
  gas?: bigint;
  wait?: boolean;
  value?: bigint;
}

/** Send a curated write and optionally wait for the receipt. */
export async function submit(ctx: Ctx, { contract, method, args, gas = GAS_DEFAULT, wait = true, value = 0n }: Write) {
  const entry = resolveContract(contract);
  const hash = await sendTx(ctx, { entry, method, args, gas, value });
  if (!wait) return ok({ txHash: hash, contract, method, status: "submitted" });
  const r = await ctx.publicClient.waitForTransactionReceipt({ hash, timeout: 180_000 });
  return ok({
    txHash: hash,
    contract,
    method,
    status: r.status,
    blockNumber: r.blockNumber.toString(),
    gasUsed: r.gasUsed.toString(),
  });
}
