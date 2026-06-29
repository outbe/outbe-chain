import { task } from "hardhat/config";

import { loadOrInitReport, appendStep } from "../../scripts/runbook/harness/report.js";
import { explorerTxUrl, resolveAddresses } from "../../scripts/runbook/harness/config.js";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const lazy = (fn: (args: any, hre: any) => Promise<void>) => async () => ({ default: fn });

/**
 * Self-test the demo harness offline: write a sample run report exercising the reporter (tx +
 * explorer link + gas + state assertion, and a LayerZero delivery proof) and the address resolver.
 * This is the E0 acceptance check and the vehicle that makes hardhat type-check the harness.
 */
const selftestAction = async () => {
  const report = loadOrInitReport("selftest", "Demo harness self-test");
  report.steps = []; // deterministic self-test: start fresh each run

  const sampleTx = `0x${"ab".repeat(32)}`;
  appendStep(report, {
    ts: new Date().toISOString(),
    phase: "selftest",
    step: "Sample BNB transaction",
    network: "bscTestnet",
    action: "Demonstrates a recorded tx: explorer link + gas + a state assertion.",
    txHash: sampleTx,
    explorerUrl: explorerTxUrl("bscTestnet", sampleTx),
    gasUsed: "123456",
    assertions: [{ label: "sample state", expected: "1", actual: "1", ok: true }],
  });

  appendStep(report, {
    ts: new Date().toISOString(),
    phase: "selftest",
    step: "Sample cross-chain message",
    network: "outbeTestnet",
    action: "Demonstrates a LayerZero delivery proof.",
    lz: { srcEid: 40812, dstEid: 40102, outboundNonce: "5", deliveredNonce: "5" },
  });

  console.log("Resolved addresses (outbeTestnet):", JSON.stringify(resolveAddresses("outbeTestnet"), null, 2));
  console.log("✅ Self-test report written to reports/selftest/report.md + report.json");
};

const selftest = task("runbook:harness-selftest", "Self-test the demo harness: write a sample run report").setAction(
  lazy(selftestAction),
);

export const selftestTasks = [selftest.build()];
