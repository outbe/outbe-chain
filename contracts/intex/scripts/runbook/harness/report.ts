// Resumable, file-based run report for the demo runbooks (QC-1261 / E0).
//
// A demo flow runs across SEPARATE `hardhat` invocations (one per phase, with real wall-clock and
// LayerZero-delivery waits between them), so the report cannot live in process memory — it is
// persisted to `reports/<runId>/` and appended on each phase. `runId` is the auction `seriesId`
// (or any caller-chosen key), so re-running a phase or resuming later keeps one coherent report.
//
// Each step records the evidence the runbook is meant to produce: tx hash + explorer link + gas,
// a state assertion (what we checked on-chain and whether it held), and — for cross-chain steps —
// the LayerZero delivery proof.

import * as fs from "fs";
import * as path from "path";

export interface StateAssertion {
  /** What was checked, e.g. "Desis stage == Started". */
  label: string;
  expected: string;
  actual: string;
  ok: boolean;
}

/** LayerZero delivery proof: the destination lazyInboundNonce caught up to the source outboundNonce. */
export interface LzProof {
  srcEid: number;
  dstEid: number;
  outboundNonce: string;
  deliveredNonce: string;
  guid?: string;
  /** Destination-chain tx hash, when resolvable. */
  destTx?: string;
}

export interface StepRecord {
  /** ISO timestamp the step was recorded. */
  ts: string;
  /** Flow name: "auction" | "settlement-qualified" | "settlement-called". */
  phase: string;
  /** Ordinal + label, e.g. "1. Desis.sendAuctionStageStart". */
  step: string;
  /** Network the tx was sent on, e.g. "outbeTestnet" | "bscTestnet". */
  network: string;
  /** Human-readable description of what the step did. */
  action: string;
  txHash?: string;
  explorerUrl?: string;
  gasUsed?: string;
  assertions?: StateAssertion[];
  lz?: LzProof;
  notes?: string;
}

export interface RunReport {
  /** Stable key for the run; the auction seriesId. */
  runId: string;
  title: string;
  startedAt: string;
  updatedAt: string;
  steps: StepRecord[];
}

const REPORTS_DIR = "reports";

const runDir = (runId: string): string => path.join(REPORTS_DIR, runId);
const jsonPath = (runId: string): string => path.join(runDir(runId), "report.json");
const mdPath = (runId: string): string => path.join(runDir(runId), "report.md");

/** Load the existing report for `runId`, or initialise a fresh one. */
export function loadOrInitReport(runId: string, title: string): RunReport {
  const p = jsonPath(runId);
  if (fs.existsSync(p)) {
    return JSON.parse(fs.readFileSync(p, "utf-8")) as RunReport;
  }
  const now = new Date().toISOString();
  return { runId, title, startedAt: now, updatedAt: now, steps: [] };
}

/** Append a step and re-persist both report.json and report.md. */
export function appendStep(report: RunReport, step: StepRecord): RunReport {
  report.steps.push(step);
  report.updatedAt = new Date().toISOString();
  persist(report);
  return report;
}

/** Write report.json + report.md to `reports/<runId>/`. */
export function persist(report: RunReport): void {
  fs.mkdirSync(runDir(report.runId), { recursive: true });
  fs.writeFileSync(jsonPath(report.runId), JSON.stringify(report, null, 2));
  fs.writeFileSync(mdPath(report.runId), renderMarkdown(report));
}

/** Render the human-readable markdown report. */
export function renderMarkdown(report: RunReport): string {
  const lines: string[] = [
    `# ${report.title}`,
    "",
    `- **Run ID (seriesId):** \`${report.runId}\``,
    `- **Started:** ${report.startedAt}`,
    `- **Updated:** ${report.updatedAt}`,
    `- **Steps recorded:** ${report.steps.length}`,
    "",
  ];

  report.steps.forEach((s, i) => {
    lines.push(`## ${i + 1}. ${s.step}  _(${s.phase} · ${s.network})_`);
    lines.push("");
    lines.push(s.action);
    lines.push("");
    if (s.txHash) {
      const link = s.explorerUrl ? `[\`${s.txHash}\`](${s.explorerUrl})` : `\`${s.txHash}\``;
      lines.push(`- **Tx:** ${link}${s.gasUsed ? ` · gas \`${s.gasUsed}\`` : ""}`);
    }
    if (s.lz) {
      const dest = s.lz.destTx ? ` · dest tx \`${s.lz.destTx}\`` : "";
      const guid = s.lz.guid ? ` · guid \`${s.lz.guid}\`` : "";
      lines.push(
        `- **LZ delivery:** eid ${s.lz.srcEid} → ${s.lz.dstEid} · ` +
          `outbound \`${s.lz.outboundNonce}\` → delivered \`${s.lz.deliveredNonce}\`${guid}${dest}`,
      );
    }
    (s.assertions ?? []).forEach((a) => {
      lines.push(`- ${a.ok ? "✅" : "❌"} **${a.label}:** expected \`${a.expected}\`, got \`${a.actual}\``);
    });
    if (s.notes) lines.push(`- ${s.notes}`);
    lines.push("");
  });

  return lines.join("\n");
}
