import type { TransactionReceipt } from "viem";
import { type Ctx, sendTx } from "../../chain.js";
import { contextNetwork } from "../../net/resolver.js";
import { receiptSummary, waitForReceipt } from "../../net/tx.js";
import { resolveContract } from "../../registry.js";
import { ok } from "../util.js";

const GAS_DEFAULT = 3_000_000n;

export interface Write {
  contract: string;
  method: string;
  args: unknown[];
  gas?: bigint;
  wait?: boolean;
  value?: bigint;
  /** What a successful receipt adds to the output, e.g. an id read from its events. */
  outcome?: (receipt: TransactionReceipt) => Record<string, unknown>;
  /** What the output reports whether or not it waits for the receipt. */
  extra?: Record<string, unknown>;
}

/** Send a curated write and optionally wait for the receipt. */
export async function submit(ctx: Ctx, write: Write) {
  const { contract, method, args, gas = GAS_DEFAULT, wait = true, value = 0n, outcome, extra } = write;
  const entry = resolveContract(contract);
  const hash = await sendTx(ctx, { entry, method, args, gas, value });
  if (!wait) return ok({ txHash: hash, contract, method, ...extra, status: "submitted" });
  const r = await waitForReceipt(contextNetwork(ctx), hash);
  return ok({
    txHash: hash,
    contract,
    method,
    ...extra,
    ...receiptSummary(r),
    ...(r.status === "success" && outcome ? outcome(r) : {}),
  });
}
