import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import type { Ctx } from "../chain.js";
import { registerAgentRewardTools } from "./sign/agentreward.js";
import { registerCredisTools } from "./sign/credis.js";
import { registerOracleTools } from "./sign/oracle.js";
import { registerStakingTools } from "./sign/staking.js";
import { registerTributeTools } from "./tribute.js";

/** Tools that sign and submit precompile transactions with the configured key. */
export function registerSignTools(server: McpServer, ctx: Ctx): void {
  registerCredisTools(server, ctx);
  registerTributeTools(server, ctx);
  registerStakingTools(server, ctx);
  registerAgentRewardTools(server, ctx);
  registerOracleTools(server, ctx);
}
