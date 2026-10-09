import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { z } from "zod";
import { type Ctx, parseNativeAmount } from "../../chain.js";
import { address, coenAmount } from "../schemas.js";
import { handler } from "../util.js";
import { submit } from "./submit.js";

/** Staking and unbonding COEN. */
export function registerStakingTools(server: McpServer, ctx: Ctx): void {
  server.tool(
    "staking_stake",
    "Stake COEN to a validator. Requires OUTBE_PRIVATE_KEY.",
    { validator: address, amount: coenAmount, wait: z.boolean().optional() },
    handler(({ validator, amount, wait }) => {
      const stake = parseNativeAmount(ctx.chain, amount);
      return submit(ctx, { contract: "staking", method: "stake", args: [validator, stake], wait, value: stake });
    }),
  );

  server.tool(
    "staking_unstake",
    "Unstake COEN (starts unbonding). Requires OUTBE_PRIVATE_KEY.",
    { amount: coenAmount, wait: z.boolean().optional() },
    handler(({ amount, wait }) =>
      submit(ctx, { contract: "staking", method: "unstake", args: [parseNativeAmount(ctx.chain, amount)], wait }),
    ),
  );

  server.tool(
    "staking_unbonded_claim",
    "Claim unbonded stake after the unbonding period. Requires OUTBE_PRIVATE_KEY.",
    { wait: z.boolean().optional() },
    handler(({ wait }) =>
      submit(ctx, { contract: "staking", method: "claimUnbonded", args: [], wait }),
    ),
  );
}
