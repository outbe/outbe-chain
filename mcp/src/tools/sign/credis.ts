import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { type Address, type TransactionReceipt, parseEventLogs, zeroAddress } from "viem";
import { z } from "zod";
import { type Ctx, parseNativeAmount, readView } from "../../chain.js";
import { ensureAllowance } from "../../net/erc20.js";
import { contextNetwork } from "../../net/resolver.js";
import { resolveContract } from "../../registry.js";
import { HEX32, address, coenAmount, waitFlag } from "../schemas.js";
import { handler } from "../util.js";
import { submit } from "./submit.js";

/** Native wei per protocol minor unit: protocol amounts carry 6 decimals, native COEN 18. */
const NATIVE_PER_PROTOCOL_UNIT = 1_000_000_000_000n;

const rawAmount = z
  .string()
  .regex(/^(0|[1-9][0-9]*)$/)
  .refine((value) => BigInt(value) < 1n << 256n, "amount exceeds uint256")
  .describe("Amount in raw token units");

/** The first `event` emitted by `contract` in `receipt`, or undefined. */
function eventArgs(receipt: TransactionReceipt, contract: string, event: string): Record<string, unknown> | undefined {
  const entry = resolveContract(contract);
  const [log] = parseEventLogs({ abi: entry.abi, logs: receipt.logs, eventName: event }).filter(
    (l) => l.address.toLowerCase() === entry.address.toLowerCase(),
  );
  return log?.args as Record<string, unknown> | undefined;
}

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
  return (result as { gratisMinor: bigint }).gratisMinor * NATIVE_PER_PROTOCOL_UNIT;
}

/** Credis reservation, Gratis pledge, issuance and repayment. */
export function registerCredisTools(server: McpServer, ctx: Ctx): void {
  server.tool(
    "credis_reserve",
    "Reserve exact stablecoin principal and freeze the Credis terms for 15 minutes. Returns the reservationId.",
    {
      smart_account: address,
      source: address.describe("Main account that pledges the Gratis collateral"),
      asset: address,
      amount: rawAmount,
      reference_currency: z.number().int().min(1).max(65535),
      wait: waitFlag,
    },
    handler(async ({ smart_account, source, asset, amount, reference_currency, wait }) =>
      submit(ctx, {
        contract: "vaultrouter",
        method: "reserveStables",
        args: [smart_account, source, asset, BigInt(amount), reference_currency],
        wait,
        outcome: (receipt) => ({
          reservationId: (eventArgs(receipt, "vaultrouter", "ReservationCreated")?.id as bigint | undefined)?.toString() ?? null,
        }),
      }),
    ),
  );
  server.tool(
    "gratis_pledge",
    "Pledge the reservation's Gratis from the caller, its source. The mac binds Pledge and the reservation's gratisMinor.",
    {
      reservation_id: rawAmount,
      mac: z.string().regex(HEX32),
      op_nonce: rawAmount.refine((v) => BigInt(v) < 1n << 64n),
      wait: waitFlag,
    },
    handler(async ({ reservation_id, mac, op_nonce, wait }) =>
      submit(ctx, {
        contract: "gratisfactory",
        method: "pledgeGratis",
        args: [BigInt(reservation_id), { mac, opNonce: BigInt(op_nonce) }],
        wait,
      }),
    ),
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
    "Issue Credis against the reservation's pledge and deliver the reserved principal. The CCA stakes native " +
      "COEN equal to the reserved Gratis collateral; omit `stake` to use exactly that. Returns the positionId.",
    { reservation_id: rawAmount, stake: coenAmount.optional(), wait: waitFlag },
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
          positionId:
            (eventArgs(receipt, "gratisfactory", "PledgeSentToCredis")?.positionId as bigint | undefined)?.toString() ??
            null,
        }),
      });
    }),
  );
  server.tool(
    "credis_settle",
    "Repay a position; released collateral returns to the source's liquid Gratis. Approves CredisFactory for " +
      "`amount` of the position's asset first when the allowance is short.",
    { position_id: rawAmount, amount: rawAmount, wait: waitFlag },
    handler(async ({ position_id, amount, wait }) => {
      const positionId = BigInt(position_id);
      const { result } = await readView(ctx, resolveContract("credis"), "getPosition", [positionId]);
      const factory = resolveContract("credisfactory").address;
      const approval = await ensureAllowance(ctx, contextNetwork(ctx), {
        token: (result as { asset: Address }).asset,
        spender: factory,
        amount: BigInt(amount),
      });
      return submit(ctx, {
        contract: "credisfactory",
        method: "settleCredis",
        args: [positionId, BigInt(amount)],
        wait,
        extra: { autoApprove: approval },
      });
    }),
  );
}
