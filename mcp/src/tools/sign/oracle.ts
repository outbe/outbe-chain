import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { z } from "zod";
import type { Ctx } from "../../chain.js";
import { address } from "../schemas.js";
import { handler } from "../util.js";
import { submit } from "./submit.js";

const GAS_VOTE = 5_000_000n;

/** Oracle feeder consent and votes. */
export function registerOracleTools(server: McpServer, ctx: Ctx): void {
  server.tool(
    "oracle_feeder_delegate",
    "Delegate oracle feeder consent to an address. Requires OUTBE_PRIVATE_KEY (validator).",
    { feeder: address, wait: z.boolean().optional() },
    handler(({ feeder, wait }) =>
      submit(ctx, { contract: "oracle", method: "delegateFeederConsent", args: [feeder], wait }),
    ),
  );

  server.tool(
    "oracle_vote_submit",
    "Submit oracle exchange-rate votes. `tuples`: [{base, quote, exchangeRate, volume}] with rate/volume as " +
      "integer minor strings (COEN/ISO rates use scale 1e6; generic pairs keep their existing scale). " +
      "Requires OUTBE_PRIVATE_KEY (validator).",
    {
      tuples: z
        .array(
          z.object({
            base: z.string(),
            quote: z.string(),
            exchangeRate: z.string(),
            volume: z.string(),
          }),
        )
        .min(1),
      wait: z.boolean().optional(),
    },
    handler(({ tuples, wait }) => {
      const t = tuples.map((x) => ({
        base: x.base,
        quote: x.quote,
        exchangeRate: BigInt(x.exchangeRate),
        volume: BigInt(x.volume),
      }));
      return submit(ctx, { contract: "oracle", method: "submitVote", args: [t], gas: GAS_VOTE, wait });
    }),
  );
}
