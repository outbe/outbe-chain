import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { type Address, type Hex, getAddress, zeroAddress } from "viem";
import { z } from "zod";
import { type Ctx, parseNativeAmount, readView } from "../../chain.js";
import { type Authorization, withAuthorization } from "../../confidential/authorize.js";
import { OP } from "../../confidential/mac.js";
import { NATIVE_UNITS_PER_PROTOCOL_UNIT } from "../../intex/units.js";
import { ensureAllowance } from "../../net/erc20.js";
import { contextNetwork } from "../../net/resolver.js";
import { requireAccount } from "../../net/tx.js";
import { resolveContract } from "../../registry.js";
import { HEX32, address, coenAmount, rawAmount, waitFlag } from "../schemas.js";
import { handler } from "../util.js";
import { eventArgs } from "./events.js";
import { submit } from "./submit.js";

/** The native COEN stake `issueCredis` requires: the reserved Gratis collateral, one for one by value. */
async function issueStake(ctx: Ctx, reservationId: bigint): Promise<bigint> {
  const pledge = (await readView(ctx, resolveContract("gratisfactory"), "pledgeOf", [reservationId])).result as [
    Address,
    bigint,
  ];
  if (pledge[0] === zeroAddress) {
    throw new Error(`reservation ${reservationId} has no pledge; pledge it with gratis_pledge first`);
  }
  const { result } = await readView(ctx, resolveContract("vaultrouter"), "reservationOf", [reservationId]);
  return (result as { gratisMinor: bigint }).gratisMinor * NATIVE_UNITS_PER_PROTOCOL_UNIT;
}

/** Credis reservation, Gratis pledge, issuance and repayment. */
export function registerCredisTools(server: McpServer, ctx: Ctx): void {
  server.tool(
    "credis_reserve",
    "Reserve exact stablecoin principal and freeze the Credis terms for 15 minutes: the pledge valuation, the call " +
      "anchor (previous closed UTC-day VWAP in the elected reference currency) and the issuance currency's policy " +
      "rate. Returns the reservationId.",
    {
      credis_owner: address.describe("Address that holds the Credis and receives the CCA's COEN stake"),
      source: address.describe("Main account that pledges the Gratis collateral"),
      asset: address,
      amount: rawAmount,
      reference_currency: z
        .number()
        .int()
        .min(1)
        .max(65535)
        .describe("ISO 4217 numeric code of the reference currency the call anchor is quoted in"),
      wait: waitFlag,
    },
    handler(async ({ credis_owner, source, asset, amount, reference_currency, wait }) =>
      submit(ctx, {
        contract: "vaultrouter",
        method: "reserveStables",
        args: [credis_owner, source, asset, BigInt(amount), reference_currency],
        wait,
        outcome: (receipt) => ({
          reservationId: (eventArgs(receipt, "vaultrouter", "ReservationCreated")?.id as bigint | undefined)?.toString() ?? null,
        }),
      }),
    ),
  );
  server.tool(
    "gratis_pledge",
    "Pledge the reservation's Gratis from the caller, its source. The Gratis modify authorization over Pledge and " +
      "the reservation's gratisMinor is computed here and the call waits for its receipt; pass mac and op_nonce " +
      "together to send your own.",
    {
      reservation_id: rawAmount,
      mac: z.string().regex(HEX32).optional(),
      op_nonce: rawAmount.refine((v) => BigInt(v) < 1n << 64n).optional(),
      wait: waitFlag,
    },
    handler(async ({ reservation_id, mac, op_nonce, wait }) => {
      const reservationId = BigInt(reservation_id);
      const pledge = (auth: Authorization, waitForReceipt: boolean | undefined, simulate = false) =>
        submit(ctx, {
          contract: "gratisfactory",
          method: "pledgeGratis",
          args: [reservationId, { mac: auth.mac, opNonce: auth.opNonce }],
          wait: waitForReceipt,
          simulate,
        });
      if (mac !== undefined || op_nonce !== undefined) {
        if (mac === undefined || op_nonce === undefined) throw new Error("pass mac and op_nonce together, or neither");
        return pledge({ mac: mac as Hex, opNonce: BigInt(op_nonce) }, wait);
      }
      const { result } = await readView(ctx, resolveContract("vaultrouter"), "reservationOf", [reservationId]);
      const reservation = result as { source: Address; gratisMinor: bigint; expiresAt: bigint };
      const signer = requireAccount(ctx).address;
      if (getAddress(reservation.source) !== signer) {
        throw new Error(`reservation ${reservationId} is pledged by ${reservation.source}, not the signer ${signer}`);
      }
      const now = (await ctx.publicClient.getBlock()).timestamp;
      if (now > BigInt(reservation.expiresAt)) throw new Error(`reservation ${reservationId} has expired`);
      const write = { ledger: "Gratis" as const, op: OP.Gratis.Pledge, amount: reservation.gratisMinor };
      return withAuthorization(ctx, contextNetwork(ctx), [write], ([auth]) => pledge(auth, true, true));
    }),
  );
  server.tool(
    "gratis_cancel_pledge",
    "Return an unused reservation pledge to the caller's liquid Gratis.",
    { reservation_id: rawAmount, wait: waitFlag },
    handler(async ({ reservation_id, wait }) =>
      submit(ctx, { contract: "gratisfactory", method: "cancelPledge", args: [BigInt(reservation_id)], wait }),
    ),
  );
  server.tool(
    "credis_issue",
    "Issue Credis against the reservation's pledge and terms and deliver the reserved principal. The CCA stakes " +
      "native COEN equal to the reserved Gratis collateral; omit `stake` to use exactly that. Returns the credisId.",
    {
      reservation_id: rawAmount,
      stake: coenAmount.optional(),
      wait: waitFlag,
    },
    handler(async ({ reservation_id, stake, wait }) => {
      const reservationId = BigInt(reservation_id);
      const value = stake === undefined ? await issueStake(ctx, reservationId) : parseNativeAmount(ctx.chain, stake);
      return submit(ctx, {
        contract: "credisfactory",
        method: "issueCredis",
        args: [reservationId],
        value,
        wait,
        outcome: (receipt) => ({
          credisId:
            (eventArgs(receipt, "credisfactory", "CredisIssued")?.credisId as bigint | undefined)?.toString() ?? null,
        }),
      });
    }),
  );
  server.tool(
    "credis_forfeit",
    "Forfeit the caller's Issued or Called Credis. Nothing is repaid: the source's remaining pledged Gratis is " +
      "burned and the outstanding principal is written off. Only the Credis owner may call it.",
    { credis_id: rawAmount, wait: waitFlag },
    handler(async ({ credis_id, wait }) =>
      submit(ctx, {
        contract: "credisfactory",
        method: "forfeitCredis",
        args: [BigInt(credis_id)],
        wait,
        outcome: (receipt) => {
          const forfeited = eventArgs(receipt, "credis", "CredisForfeited");
          return {
            gratisBurnedMinor: (forfeited?.gratisBurnedMinor as bigint | undefined)?.toString() ?? null,
            principalWrittenOffMinor: (forfeited?.principalWrittenOffMinor as bigint | undefined)?.toString() ?? null,
          };
        },
      }),
    ),
  );
  server.tool(
    "credis_settle",
    "Repay a Credis; released collateral returns to the source's liquid Gratis. Approves CredisFactory for " +
      "`amount` of the position's asset first when the allowance is short.",
    { credis_id: rawAmount, amount: rawAmount, wait: waitFlag },
    handler(async ({ credis_id, amount, wait }) => {
      const credisId = BigInt(credis_id);
      const { result } = await readView(ctx, resolveContract("credis"), "getCredis", [credisId]);
      const factory = resolveContract("credisfactory").address;
      const approval = await ensureAllowance(ctx, contextNetwork(ctx), {
        token: (result as { asset: Address }).asset,
        spender: factory,
        amount: BigInt(amount),
      });
      return submit(ctx, {
        contract: "credisfactory",
        method: "settleCredis",
        args: [credisId, BigInt(amount)],
        wait,
        extra: { autoApprove: approval },
      });
    }),
  );
}
