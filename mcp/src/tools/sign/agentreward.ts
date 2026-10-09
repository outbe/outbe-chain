import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { z } from "zod";
import { type Ctx, parseNativeAmount } from "../../chain.js";
import { coenAmount } from "../schemas.js";
import { handler } from "../util.js";
import { submit } from "./submit.js";

/** Claiming AgentReward as a Gem. */
export function registerAgentRewardTools(server: McpServer, ctx: Ctx): void {
  // The Rewards precompile (EE03) exposes no callable methods. Validator
  // emission is paid in gems (crates/system/rewards/src/precompile.rs).
  server.tool(
    "agentreward_claim",
    "Claim AgentReward from one pool (0 = WAA, 1 = SRA, 2 = CCA) as a Gem. Omit amount to claim the whole pool balance. Requires OUTBE_PRIVATE_KEY.",
    {
      pool: z.number().int().min(0).max(2).describe("0 = WAA, 1 = SRA, 2 = CCA"),
      amount: coenAmount.optional().describe("amount to claim; omit for the whole balance"),
      wait: z.boolean().optional(),
    },
    handler(({ pool, amount, wait }) =>
      submit(ctx, {
        contract: "agentreward",
        method: "claimReward",
        args: [pool, amount === undefined ? 0n : parseNativeAmount(ctx.chain, amount)],
        wait,
      }),
    ),
  );
}
