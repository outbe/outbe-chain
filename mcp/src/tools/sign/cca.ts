import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { z } from "zod";
import { type Ctx, parseNativeAmount } from "../../chain.js";
import { coenAmount, rawAmount, waitFlag } from "../schemas.js";
import { handler } from "../util.js";
import { eventArgs, eventId } from "./events.js";
import { submit } from "./submit.js";

const CCA_STATES = ["Bonding", "Active", "Deregistering", "Deregistered"];

/** The caller's CCA bond in the registry, and returning an unused Credis liquidity reservation. */
export function registerCcaTools(server: McpServer, ctx: Ctx): void {
  server.tool(
    "cca_bond",
    "Bond `amount` COEN as a CCA under `name`. The CCA turns Active once its total bond reaches the registry " +
      "requirement; bonding is refused while an unbond is pending.",
    { name: z.string().min(1).describe("CCA display name"), amount: coenAmount, wait: waitFlag },
    handler(async ({ name, amount, wait }) =>
      submit(ctx, {
        contract: "ccaregistry",
        method: "bond",
        args: [name],
        value: parseNativeAmount(ctx.chain, amount),
        wait,
        outcome: (receipt) => {
          const state = eventArgs(receipt, "ccaregistry", "Bonded")?.state as number | undefined;
          return { state: state === undefined ? null : CCA_STATES[state] };
        },
      }),
    ),
  );

  server.tool(
    "cca_unbond",
    "Start unbonding the caller's whole CCA bond. The CCA stops being Active at once; the bond can be " +
      "claimed with cca_unbonded_claim after the cooldown (`unbondUnlocksAfter`, unix seconds).",
    { wait: waitFlag },
    handler(async ({ wait }) =>
      submit(ctx, {
        contract: "ccaregistry",
        method: "unbond",
        args: [],
        wait,
        outcome: (receipt) => ({
          unbondUnlocksAfter: eventId(receipt, "ccaregistry", "UnbondRequested", "unbondUnlocksAfter"),
        }),
      }),
    ),
  );

  server.tool(
    "cca_unbonded_claim",
    "Withdraw the caller's unbonded CCA bond to its wallet once the unbond cooldown has passed.",
    { wait: waitFlag },
    handler(async ({ wait }) =>
      submit(ctx, {
        contract: "ccaregistry",
        method: "claimUnbonded",
        args: [],
        wait,
        outcome: (receipt) => ({ amountWei: eventId(receipt, "ccaregistry", "UnbondClaimed", "amount") }),
      }),
    ),
  );

  server.tool(
    "credis_reservation_return",
    "Deposit a liquidity reservation's assets back into their vault. Its CCA may return it at any time, " +
      "anyone once it has expired; an already settled reservation is a no-op.",
    { reservation_id: rawAmount, wait: waitFlag },
    handler(async ({ reservation_id, wait }) =>
      submit(ctx, {
        contract: "vaultrouter",
        method: "returnReservation",
        args: [BigInt(reservation_id)],
        wait,
        outcome: (receipt) => ({
          mintedShares: eventId(receipt, "vaultrouter", "ReservationReturned", "mintedShares"),
        }),
      }),
    ),
  );
}
