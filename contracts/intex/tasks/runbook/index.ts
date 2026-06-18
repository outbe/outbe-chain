// Aggregated demo-runbook tasks (QC-1261). Registered once in config/hardhat.config.ts.
// E0 (harness self-test) lands here; E1 (auction) / E2 (qualified) / E3 (called) append their tasks.

import { selftestTasks } from "./selftest.js";
import { auctionDemoTasks } from "./auction.js";

export const runbookTasks = [...selftestTasks, ...auctionDemoTasks];
