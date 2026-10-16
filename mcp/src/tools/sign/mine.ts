import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import type { Ctx } from "../../chain.js";
import { type Write, withAuthorization } from "../../confidential/authorize.js";
import { plain, signerBalance } from "../../confidential/balances.js";
import { type Ledger, OP } from "../../confidential/mac.js";
import { contextNetwork } from "../../net/resolver.js";
import { rawAmount } from "../schemas.js";
import { handler } from "../util.js";
import { eventId } from "./events.js";
import { submit } from "./submit.js";

const protocolAmount = rawAmount.describe("amount in six-decimal minor units");

/** Refuses before any authorization when the signer's `ledger` balance cannot cover `amount`. */
async function ensureBalance(ctx: Ctx, ledger: Ledger, amount: bigint): Promise<void> {
  const balance = await signerBalance(ctx, contextNetwork(ctx), ledger);
  if (balance < amount) {
    throw new Error(`${ledger} balance ${plain(balance).value} is below the requested ${plain(amount).value}`);
  }
}

/** Burns the signer's confidential Promis or Gratis into COEN or Gratis. */
export function registerMineTools(server: McpServer, ctx: Ctx): void {
  const mine = async (
    contract: string,
    method: string,
    amount: bigint,
    writes: Write[],
    outcome: { contract: string; event: string; field: string },
  ) => {
    await ensureBalance(ctx, writes[0].ledger, amount);
    return withAuthorization(ctx, contextNetwork(ctx), writes, (auths) =>
      submit(ctx, {
        contract,
        method,
        args: [amount, ...auths.flatMap((auth) => [auth.mac, auth.opNonce])],
        simulate: true,
        outcome: (receipt) => ({ [outcome.field]: eventId(receipt, outcome.contract, outcome.event, outcome.field) }),
      }),
    );
  };

  server.tool(
    "promis_coen_mine",
    "Burn `amount` of your confidential Promis and mine COEN to your wallet. The Promis modify authorization " +
      "is computed here and the balance is checked first. Waits for the receipt. Requires OUTBE_PRIVATE_KEY.",
    { amount: protocolAmount },
    handler(async ({ amount }) => {
      const value = BigInt(amount);
      return mine("promisfactory", "mineCoen", value, [{ ledger: "Promis", op: OP.Promis.Burn, amount: value }], {
        contract: "promisfactory",
        event: "CoenMined",
        field: "coenMinor",
      });
    }),
  );

  server.tool(
    "promis_gratis_mine",
    "Burn `amount` of your confidential Promis and mint the same amount of Gratis to you. Both modify " +
      "authorizations are computed here and the Promis balance is checked first. Waits for the receipt. " +
      "Requires OUTBE_PRIVATE_KEY.",
    { amount: protocolAmount },
    handler(async ({ amount }) => {
      const value = BigInt(amount);
      return mine(
        "promisfactory",
        "mineGratis",
        value,
        [
          { ledger: "Promis", op: OP.Promis.Burn, amount: value },
          { ledger: "Gratis", op: OP.Gratis.Mint, amount: value },
        ],
        { contract: "gratis", event: "GratisMinted", field: "amount" },
      );
    }),
  );

  server.tool(
    "gratis_coen_mine",
    "Burn `amount` of your liquid Gratis and mine COEN to your wallet. The Gratis modify authorization is " +
      "computed here and the balance is checked first. Waits for the receipt. Requires OUTBE_PRIVATE_KEY.",
    { amount: protocolAmount },
    handler(async ({ amount }) => {
      const value = BigInt(amount);
      return mine("gratisfactory", "mineCoen", value, [{ ledger: "Gratis", op: OP.Gratis.Burn, amount: value }], {
        contract: "gratisfactory",
        event: "CoenMined",
        field: "coenMinor",
      });
    }),
  );
}
