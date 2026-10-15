import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { type Address, getAddress } from "viem";
import { type Ctx, readView } from "../../chain.js";
import { withAuthorization } from "../../confidential/authorize.js";
import { OP } from "../../confidential/mac.js";
import { type EncryptedNod, signerNodAmount } from "../../confidential/nod.js";
import { contextNetwork } from "../../net/resolver.js";
import { requireAccount } from "../../net/tx.js";
import { grindMiningNonce } from "../../pow.js";
import { resolveContract } from "../../registry.js";
import { address, rawAmount, waitFlag } from "../schemas.js";
import { handler, ok } from "../util.js";
import { powDifficulty } from "./gem.js";
import { quoteRightSettlement, settleRight } from "./settle.js";
import { submit } from "./submit.js";

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

  server.tool(
    "nod_gratis_mine",
    "Burn your Settled Nod and mint its Gratis amount to you. The amount is opened with your creator key; " +
      "pass `gratis_amount` for a Nod whose Tribute was offered under another key. The proof of work and the " +
      "Gratis modify authorization are computed here. Waits for the receipt. Requires OUTBE_PRIVATE_KEY.",
    { nod_id: rawAmount, gratis_amount: rawAmount.optional() },
    handler(async ({ nod_id, gratis_amount }) => {
      const nodId = BigInt(nod_id);
      const network = contextNetwork(ctx);
      const owner = requireAccount(ctx).address;
      const { result } = await readView(ctx, resolveContract("nod"), "getNod", [nodId]);
      const nod = result as EncryptedNod & { isSettled: boolean };
      if (getAddress(nod.owner) !== owner) throw new Error(`nod ${nodId} belongs to ${nod.owner}, not the signer ${owner}`);
      if (!nod.isSettled) throw new Error(`nod ${nodId} is not Settled; settle it with nod_settle first`);
      const amount = gratis_amount === undefined ? await signerNodAmount(ctx, network, nod) : BigInt(gratis_amount);
      const nonce = grindMiningNonce("Nod", nodId, owner, await powDifficulty(ctx));
      const write = { ledger: "Gratis" as const, op: OP.Gratis.Mint, amount };
      return withAuthorization(ctx, network, [write], ([auth]) =>
        submit(ctx, {
          contract: "nodfactory",
          method: "mineGratis",
          args: [nodId, nonce, auth.mac, auth.opNonce],
          simulate: true,
          extra: { gratisMinor: amount.toString(), powNonce: nonce.toString() },
        }),
      );
    }),
  );
}
