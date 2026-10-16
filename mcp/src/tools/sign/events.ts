import { type TransactionReceipt, parseEventLogs } from "viem";
import { resolveContract } from "../../registry.js";

/** The first `event` emitted by `contract` in `receipt`, or undefined. */
export function eventArgs(receipt: TransactionReceipt, contract: string, event: string): Record<string, unknown> | undefined {
  const entry = resolveContract(contract);
  const [log] = parseEventLogs({ abi: entry.abi, logs: receipt.logs, eventName: event }).filter(
    (l) => l.address.toLowerCase() === entry.address.toLowerCase(),
  );
  return log?.args as Record<string, unknown> | undefined;
}

/** `field` of the first `event` as a decimal string, or null when the receipt carries none. */
export function eventId(receipt: TransactionReceipt, contract: string, event: string, field: string): string | null {
  return (eventArgs(receipt, contract, event)?.[field] as bigint | undefined)?.toString() ?? null;
}
