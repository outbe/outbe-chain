import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { type Address, getAddress } from "viem";
import { type Ctx, readView } from "../../chain.js";
import { withAuthorization } from "../../confidential/authorize.js";
import { OP } from "../../confidential/mac.js";
import { FACTORY_ABI, intexAddress } from "../../intex/registry.js";
import { contextNetwork } from "../../net/resolver.js";
import { requireAccount } from "../../net/tx.js";
import { grindMiningNonce } from "../../pow.js";
import { ownedTokenIds, view } from "../../read.js";
import { resolveContract } from "../../registry.js";
import { seriesArg } from "../intex/args.js";
import { address, rawAmount, waitFlag } from "../schemas.js";
import { handler, ok } from "../util.js";
import { eventId } from "./events.js";
import { settleRight } from "./settle.js";
import { submit } from "./submit.js";

const GEM_SETTLED = 3;

/** The proof-of-work difficulty every right mines at, read from the chain. */
export async function powDifficulty(ctx: Ctx): Promise<number> {
  const network = contextNetwork(ctx);
  return await network.client.readContract({
    address: intexAddress(network, "factory"),
    abi: FACTORY_ABI,
    functionName: "powDifficulty",
  });
}

/** Gem positions, merchant issuance, settlement and Promis mining. */
export function registerGemTools(server: McpServer, ctx: Ctx): void {
  server.tool(
    "gem_position_issue",
    "Send your Issued Intex units of a series into a new Gem position you, as merchant, issue Gems from. " +
      "Returns the positionId.",
    { series: seriesArg, units: rawAmount, wait: waitFlag },
    handler(async ({ series, units, wait }) =>
      submit(ctx, {
        contract: "gemfactory",
        method: "issueGemPosition",
        args: [series, BigInt(units)],
        wait,
        outcome: (receipt) => ({ positionId: eventId(receipt, "gemfactory", "GemPositionIssued", "positionId") }),
      }),
    ),
  );

  server.tool(
    "gem_issue",
    "Issue a Gem to `owner` from your Gem position, carrying `promis_load` (six-decimal minor units) of its " +
      "remaining capacity. Only the position's merchant may call it. Returns the gemId.",
    { position_id: rawAmount, owner: address, promis_load: rawAmount, wait: waitFlag },
    handler(async ({ position_id, owner, promis_load, wait }) =>
      submit(ctx, {
        contract: "gemfactory",
        method: "issueGem",
        args: [BigInt(position_id), getAddress(owner), BigInt(promis_load)],
        wait,
        outcome: (receipt) => ({ gemId: eventId(receipt, "gemfactory", "GemIssued", "gemId") }),
      }),
    ),
  );

  server.tool(
    "gem_settle",
    "Pay a Gem's settlement cost in `asset`: quotes it, approves GemFactory for that amount if the allowance " +
      "is short, and settles at the quoted snapshot. Anyone may pay; the Gem stays with its owner.",
    { gem_id: rawAmount, asset: address, wait: waitFlag },
    handler(async ({ gem_id, asset, wait }) => settleRight(ctx, "gemfactory", "settleGem", BigInt(gem_id), asset as Address, wait)),
  );

  server.tool(
    "gem_promis_mine",
    "Burn your Settled Gem and mine its Promis load to your wallet. The proof of work and the Promis modify " +
      "authorization are computed here. Waits for the receipt. Requires OUTBE_PRIVATE_KEY.",
    { gem_id: rawAmount },
    handler(async ({ gem_id }) => {
      const gemId = BigInt(gem_id);
      const signer = requireAccount(ctx).address;
      const { result } = await readView(ctx, resolveContract("gem"), "getGem", [gemId]);
      const gem = result as { owner: Address; state: number; promisLoadMinor: bigint };
      if (getAddress(gem.owner) !== signer) throw new Error(`gem ${gemId} belongs to ${gem.owner}, not the signer ${signer}`);
      if (Number(gem.state) !== GEM_SETTLED) throw new Error(`gem ${gemId} is not Settled; settle it with gem_settle first`);
      const nonce = grindMiningNonce("Gem", gemId, signer, await powDifficulty(ctx));
      const write = { ledger: "Promis" as const, op: OP.Promis.Mint, amount: gem.promisLoadMinor };
      return withAuthorization(ctx, contextNetwork(ctx), [write], ([auth]) =>
        submit(ctx, {
          contract: "gemfactory",
          method: "minePromis",
          args: [gemId, nonce, auth.mac, auth.opNonce],
          simulate: true,
          extra: { promisMinor: gem.promisLoadMinor.toString(), powNonce: nonce.toString() },
        }),
      );
    }),
  );

  server.tool(
    "gem_positions_by_owner",
    "Gem positions a merchant holds, with their remaining capacity and expiry.",
    { owner: address },
    handler(async ({ owner }) => {
      const ids = await ownedTokenIds(ctx, "gemfactory", owner);
      const positions = await Promise.all(ids.map((id) => view(ctx, "gemfactory", "getPosition", [id])));
      return ok({ owner, count: positions.length, positions });
    }),
  );
}
