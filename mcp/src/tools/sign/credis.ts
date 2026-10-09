import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { z } from "zod";
import { type Ctx, parseNativeAmount } from "../../chain.js";
import { HEX32, address, coenAmount } from "../schemas.js";
import { handler } from "../util.js";
import { submit } from "./submit.js";

/** Credis reservation, Gratis pledge, issuance and repayment. */
export function registerCredisTools(server: McpServer, ctx: Ctx): void {
  const rawAmount = z.string().regex(/^(0|[1-9][0-9]*)$/).refine(
    value => BigInt(value) < (1n << 256n), "amount exceeds uint256",
  ).describe("Amount in raw token units");
  server.tool("credis_reserve", "Reserve exact stablecoin principal and freeze the Credis terms for 15 minutes.",
    { smart_account: address, source: address.describe("Main account that pledges the Gratis collateral"), asset: address, amount: rawAmount, reference_currency: z.number().int().min(1).max(65535) },
    handler(async ({ smart_account, source, asset, amount, reference_currency }) =>
      submit(ctx, { contract: "vaultrouter", method: "reserveStables", args: [smart_account, source, asset, BigInt(amount), reference_currency] })));
  server.tool("gratis_pledge", "Pledge the reservation's Gratis from the caller, its source. The mac binds Pledge and the reservation's gratisMinor.",
    { reservation_id: rawAmount, mac: z.string().regex(HEX32), op_nonce: rawAmount.refine(v => BigInt(v) < (1n << 64n)) },
    handler(async ({ reservation_id, mac, op_nonce }) =>
      submit(ctx, { contract: "gratisfactory", method: "pledgeGratis", args: [BigInt(reservation_id), { mac, opNonce: BigInt(op_nonce) }] })));
  server.tool("gratis_cancel_pledge", "Return an unused reservation pledge to the caller's liquid Gratis.",
    { reservation_id: rawAmount },
    handler(async ({ reservation_id }) =>
      submit(ctx, { contract: "gratisfactory", method: "cancelPledge", args: [BigInt(reservation_id)] })));
  server.tool("credis_issue", "Issue Credis against the reservation's pledge and deliver the reserved principal.",
    { reservation_id: rawAmount, stake: coenAmount },
    handler(async ({ reservation_id, stake }) =>
      submit(ctx, { contract: "credisfactory", method: "issueCredis", args: [BigInt(reservation_id)], value: parseNativeAmount(ctx.chain, stake) })));
  server.tool("credis_settle", "Repay a position after approving its asset to CredisFactory; released collateral returns to the source's liquid Gratis.",
    { position_id: rawAmount, amount: rawAmount },
    handler(async ({ position_id, amount }) =>
      submit(ctx, { contract: "credisfactory", method: "settleCredis", args: [BigInt(position_id), BigInt(amount)] })));
}
