import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import type { Address } from "viem";
import type { Ctx } from "../../chain.js";
import { address, rawAmount, waitFlag } from "../schemas.js";
import { handler, ok } from "../util.js";
import { quoteRightSettlement, settleRight } from "./settle.js";

/** Nod settlement. */
export function registerNodTools(server: McpServer, ctx: Ctx): void {
  server.tool(
    "nod_settle_quote",
    "What settling a Nod with `asset` costs: the payment in the asset's minor units, the currency it is " +
      "denominated in and the VWAP snapshot it converts at (stale at the next hourly cutoff).",
    { nod_id: rawAmount, asset: address },
    handler(async ({ nod_id, asset }) => {
      const quote = await quoteRightSettlement(ctx, "nodfactory", BigInt(nod_id), asset as Address);
      return ok({
        nodId: nod_id,
        asset,
        settlementCurrency: quote.settlementCurrency,
        paymentMinor: quote.paymentMinor.toString(),
        snapshotId: quote.snapshotId.toString(),
      });
    }),
  );

  server.tool(
    "nod_settle",
    "Pay a Nod's settlement cost in `asset`: quotes it, approves NodFactory for that amount if the allowance " +
      "is short, and settles at the quoted snapshot. Anyone may pay; the Nod stays with its owner.",
    { nod_id: rawAmount, asset: address, wait: waitFlag },
    handler(async ({ nod_id, asset, wait }) => settleRight(ctx, "nodfactory", "settleNod", BigInt(nod_id), asset as Address, wait)),
  );
}
