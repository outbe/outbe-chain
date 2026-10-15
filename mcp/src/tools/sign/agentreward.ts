import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { z } from "zod";
import { type Ctx, parseNativeAmount } from "../../chain.js";
import { coenAmount } from "../schemas.js";
import { handler } from "../util.js";
import { eventId } from "./events.js";
import { submit } from "./submit.js";

/** Claiming AgentReward as a Gem. */
export function registerAgentRewardTools(server: McpServer, ctx: Ctx): void {
  // The Rewards precompile (EE03) exposes no callable methods. Validator
  // emission is paid in gems (crates/system/rewards/src/precompile.rs).
  server.tool(
    "agentreward_claim",
    "Claim AgentReward from one pool (0 = WAA, 1 = SRA, 2 = CCA) as a Gem in the chosen issuance and reference " +
      "currencies. The entry price is the previous UTC day's COEN VWAP in the reference currency. Omit amount to " +
      "claim the whole pool balance. Returns the gemId. Requires OUTBE_PRIVATE_KEY.",
    {
      pool: z.number().int().min(0).max(2).describe("0 = WAA, 1 = SRA, 2 = CCA"),
      amount: coenAmount.optional().describe("amount to claim; omit for the whole balance"),
      issuance_currency: z.number().int().min(1).max(999).describe("issuance currency, ISO 4217 numeric"),
      reference_currency: z
        .number()
        .int()
        .min(1)
        .max(999)
        .describe("reference currency, ISO 4217 numeric; must be an Oracle reference currency priced for the previous UTC day"),
      wait: z.boolean().optional(),
    },
    handler(({ pool, amount, issuance_currency, reference_currency, wait }) =>
      submit(ctx, {
        contract: "agentreward",
        method: "claimReward",
        args: [
          pool,
          amount === undefined ? 0n : parseNativeAmount(ctx.chain, amount),
          issuance_currency,
          reference_currency,
        ],
        wait,
        outcome: (receipt) => ({ gemId: eventId(receipt, "gemfactory", "GemIssued", "gemId") }),
      }),
    ),
  );
}
