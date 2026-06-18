// Aggregated runbook tasks. Registered once in config/hardhat.config.ts.

import { selftestTasks } from "./selftest.js";
import { auctionDemoTasks } from "./auction.js";

export const runbookTasks = [...selftestTasks, ...auctionDemoTasks];
