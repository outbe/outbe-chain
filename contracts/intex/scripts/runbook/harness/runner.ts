// Step-runner for the demo runbooks (QC-1261 / E0).
//
// Wraps one logical step of a flow: run the action, capture the tx receipt (gas), build the
// explorer link, and append the evidence to the resumable run report. A "step" returns whatever
// evidence it produced (tx hash, on-chain state assertions, LZ delivery proof); the runner adds the
// gas/explorer wrapping and the timestamp.

import { type PublicClient } from "viem";
import { type DemoNetwork, explorerTxUrl } from "./config.js";
import { type RunReport, type StepRecord, type StateAssertion, type LzProof, appendStep } from "./report.js";

export interface StepResult {
  txHash?: `0x${string}`;
  assertions?: StateAssertion[];
  lz?: LzProof;
  notes?: string;
}

export interface StepContext {
  report: RunReport;
  /** Network the step's tx is sent on. */
  network: DemoNetwork;
  /** Flow name recorded on the step, e.g. "auction". */
  phase: string;
  /** Public client on `network`, used to fetch the receipt for gas. */
  publicClient: PublicClient;
}

/** Build a `StateAssertion`, comparing string-coerced expected/actual. */
export function assertState(label: string, expected: unknown, actual: unknown): StateAssertion {
  const e = String(expected);
  const a = String(actual);
  return { label, expected: e, actual: a, ok: e === a };
}

/**
 * Run one labeled step: execute `fn`, capture gas from the receipt if it produced a tx, build the
 * explorer link, and append the recorded evidence to the report. Returns the step's result.
 */
export async function runStep(
  ctx: StepContext,
  step: string,
  action: string,
  fn: () => Promise<StepResult>,
): Promise<StepResult> {
  console.log(`\n=== ${step}  (${ctx.phase} · ${ctx.network}) ===`);
  console.log(`    ${action}`);

  const result = await fn();

  let gasUsed: string | undefined;
  let explorerUrl: string | undefined;
  if (result.txHash) {
    explorerUrl = explorerTxUrl(ctx.network, result.txHash);
    try {
      const receipt = await ctx.publicClient.waitForTransactionReceipt({ hash: result.txHash });
      gasUsed = receipt.gasUsed.toString();
    } catch {
      // The action may have already awaited the receipt; gas is best-effort evidence.
    }
    console.log(`    tx: ${result.txHash}${explorerUrl ? ` (${explorerUrl})` : ""}`);
  }

  const record: StepRecord = {
    ts: new Date().toISOString(),
    phase: ctx.phase,
    step,
    network: ctx.network,
    action,
    txHash: result.txHash,
    explorerUrl,
    gasUsed,
    assertions: result.assertions,
    lz: result.lz,
    notes: result.notes,
  };
  appendStep(ctx.report, record);

  (result.assertions ?? []).forEach((a) => console.log(`    ${a.ok ? "✅" : "❌"} ${a.label}: got ${a.actual}`));
  if (result.lz) console.log(`    lz: ${result.lz.srcEid}->${result.lz.dstEid} delivered ${result.lz.deliveredNonce}`);

  return result;
}
