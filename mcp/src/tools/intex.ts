import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import type { Ctx } from "../chain.js";
import { registerAuctionTools } from "./intex/auctions.js";
import { registerBidTools } from "./intex/bids.js";
import { registerBridgeTools } from "./intex/bridge.js";
import { intexDeps } from "./intex/deps.js";
import { registerFundingTools } from "./intex/funding.js";
import { registerHoldingsTools } from "./intex/holdings.js";
import { registerSeriesTools } from "./intex/series.js";
import { registerSettlementTools } from "./intex/settlement.js";

export { wcoenLockAmount } from "../intex/units.js";

/**
 * Intex participant tools: the series ledger, NFT holdings, auctions and bids,
 * escrow funding, the bridge to outbe, and settlement/Promis on outbe. Read
 * tools work without a key. Signing tools require OUTBE_PRIVATE_KEY.
 */
export function registerIntexTools(server: McpServer, ctx: Ctx): void {
  const deps = intexDeps(ctx);
  registerSeriesTools(server, deps);
  registerHoldingsTools(server, deps);
  registerAuctionTools(server, deps);
  registerBidTools(server, deps);
  registerFundingTools(server, deps);
  registerBridgeTools(server, deps);
  registerSettlementTools(server, deps);
}
