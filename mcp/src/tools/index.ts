import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import type { Ctx } from "../chain.js";
import { registerIntentTools } from "./intent.js";
import { registerIntexTools } from "./intex.js";
import { registerRpcTools } from "./rpc.js";
import { registerSignTools } from "./sign.js";
import { registerViewTools } from "./view.js";

export function registerTools(server: McpServer, ctx: Ctx): void {
  registerRpcTools(server, ctx);
  registerViewTools(server, ctx);
  registerSignTools(server, ctx);
  registerIntentTools(server, ctx);
  registerIntexTools(server, ctx);
}
