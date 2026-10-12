import type { Account, Address, Hex, TransactionReceipt } from "viem";
import type { Ctx } from "../chain.js";
import type { Network } from "./resolver.js";

const RECEIPT_TIMEOUT_MS = 180_000;

export interface Call {
  to: Address;
  data: Hex;
  value: bigint;
}

export function requireAccount(ctx: Ctx): Account {
  if (!ctx.account) {
    throw new Error("signing requires a key - set OUTBE_PRIVATE_KEY in the MCP server env");
  }
  return ctx.account;
}

/** The node's gas estimate plus a 30% margin. */
async function estimateGas(ctx: Ctx, network: Network, call: Call): Promise<bigint> {
  const estimate = await network.client.estimateGas({ account: ctx.account?.address, ...call });
  return (estimate * 130n) / 100n;
}

/** Signs `call` with the configured key and sends it under an estimated gas limit. */
export async function sendCall(ctx: Ctx, network: Network, call: Call): Promise<Hex> {
  const gas = await estimateGas(ctx, network, call);
  const account = requireAccount(ctx);
  if (!network.wallet) throw new Error(`no signer for ${network.name}`);
  return network.wallet.sendTransaction({ account, chain: network.chain, ...call, gas });
}

export function waitForReceipt(network: Network, hash: Hex): Promise<TransactionReceipt> {
  return network.client.waitForTransactionReceipt({ hash, timeout: RECEIPT_TIMEOUT_MS });
}

export function receiptSummary(receipt: TransactionReceipt) {
  return {
    status: receipt.status,
    blockNumber: receipt.blockNumber.toString(),
    gasUsed: receipt.gasUsed.toString(),
  };
}
