import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { z } from "zod";
import type { Ctx } from "../chain.js";
import { CONTRACTS } from "../registry.js";
import { handler, ok, view } from "./util.js";

const addr = z.string().describe("0x-prefixed address");
const wwd = z.number().int().describe("WorldwideDay as YYYYMMDD, e.g. 20260601");

export function registerViewTools(server: McpServer, ctx: Ctx): void {
  // --- generic escape hatch: any view method of any precompile ---------------
  server.tool(
    "contract_call",
    `Call any view/pure method on an outbe precompile and get decoded, human-readable output. ` +
      `\`contract\` is a registry name (${Object.keys(CONTRACTS).join(", ")}) or a 0x address. ` +
      `\`method\` is the function name; \`args\` are its arguments in order (numbers/strings).`,
    {
      contract: z.string(),
      method: z.string(),
      args: z.array(z.any()).optional(),
    },
    handler(async ({ contract, method, args }) =>
      ok(await view(ctx, contract, method, args ?? [])),
    ),
  );

  // --- Tribute ---------------------------------------------------------------
  server.tool(
    "tribute_get",
    "Tribute metadata by token id: owner + decoded attributes (worldwide_day, currency, amounts).",
    { id: z.string().describe("Tribute token id (decimal or 0x hex)") },
    handler(async ({ id }) => {
      const tokenId = BigInt(id);
      const [metadata, owner] = await Promise.all([
        view(ctx, "tribute", "tokenURI", [tokenId]),
        view(ctx, "tribute", "ownerOf", [tokenId]),
      ]);
      return ok({ tokenId: tokenId.toString(), owner, metadata });
    }),
  );

  server.tool(
    "tributes_by_owner",
    "List Tribute token ids owned by an address.",
    { owner: addr },
    handler(async ({ owner }) => ok(await view(ctx, "tribute", "getTributesByOwner", [owner]))),
  );

  server.tool(
    "tributes_by_day",
    "List Tribute token ids recorded for a WorldwideDay.",
    { worldwide_day: wwd },
    handler(async ({ worldwide_day }) =>
      ok(await view(ctx, "tribute", "getTributesByDay", [worldwide_day])),
    ),
  );

  server.tool(
    "tribute_day_totals",
    "Aggregate Tribute totals for a WorldwideDay (count, nominal amount, sealed).",
    { worldwide_day: wwd },
    handler(async ({ worldwide_day }) =>
      ok(await view(ctx, "tribute", "getDayTotals", [worldwide_day])),
    ),
  );

  // --- Nod -------------------------------------------------------------------
  server.tool(
    "nod_get",
    "Nod NFT data by token id (decoded) plus parsed tokenURI metadata.",
    { id: z.string().describe("Nod token id (decimal or 0x hex)") },
    handler(async ({ id }) => {
      const nodId = BigInt(id);
      const [data, metadata] = await Promise.all([
        view(ctx, "nod", "nodData", [nodId]),
        view(ctx, "nod", "tokenURI", [nodId]),
      ]);
      return ok({ nodId: nodId.toString(), data, metadata });
    }),
  );

  server.tool(
    "nods_by_owner",
    "List Nod token ids owned by an address.",
    { owner: addr },
    handler(async ({ owner }) => ok(await view(ctx, "nod", "tokens", [owner]))),
  );

  // --- Gem -------------------------------------------------------------------
  server.tool(
    "gem_get",
    "Gem NFT status by token id (decoded: type, state, load, prices, currency, issued).",
    { id: z.string().describe("Gem token id (decimal or 0x hex)") },
    handler(async ({ id }) => ok(await view(ctx, "gem", "getGemStatus", [BigInt(id)]))),
  );

  server.tool(
    "gems_by_owner",
    "List Gems owned by an address with decoded status for each (Gem has no bulk getter, " +
      "so this enumerates balanceOf -> tokenOfOwnerByIndex -> getGemStatus).",
    { owner: addr },
    handler(async ({ owner }) => {
      const balance = Number(await view(ctx, "gem", "balanceOf", [owner]));
      const gems: unknown[] = [];
      for (let i = 0; i < balance; i++) {
        const tokenId = await view(ctx, "gem", "tokenOfOwnerByIndex", [owner, i]);
        gems.push(await view(ctx, "gem", "getGemStatus", [BigInt(tokenId as string)]));
      }
      return ok({ owner, count: balance, gems });
    }),
  );

  // --- Balances --------------------------------------------------------------
  server.tool(
    "gratis_balance",
    "Gratis balance + pledged amount for an account (in COEN).",
    { account: addr },
    handler(async ({ account }) => {
      const [balance, pledged] = await Promise.all([
        view(ctx, "gratis", "balanceOf", [account]),
        view(ctx, "gratis", "pledgedOf", [account]),
      ]);
      return ok({ account, balance, pledged });
    }),
  );

  server.tool(
    "promis_balance",
    "Promis balance for an account (in COEN).",
    { account: addr },
    handler(async ({ account }) => ok(await view(ctx, "promis", "balanceOf", [account]))),
  );

  server.tool(
    "fidelity_rcfi",
    "Fidelity RCFI (Retention Component of Fidelity Index) for an account, in decayed days.",
    { account: addr },
    handler(async ({ account }) => ok(await view(ctx, "fidelity", "getRcfi", [account]))),
  );

  server.tool(
    "agentreward_claimable",
    "Claimable AgentReward balance for an account (in COEN).",
    { account: addr },
    handler(async ({ account }) =>
      ok(await view(ctx, "agentreward", "getClaimableBalance", [account])),
    ),
  );

  // --- Metadosis / WorldwideDay ---------------------------------------------
  server.tool(
    "offering_days",
    "WorldwideDays currently in OFFERING status (the days a tribute offer can target).",
    {},
    handler(async () => {
      const wwds = (await view(ctx, "metadosis", "getWorldwideDaysByStatus", [2])) as {
        wwds: { wwd: number; date: string }[];
      };
      return ok(wwds);
    }),
  );

  server.tool(
    "worldwide_day",
    "Full lifecycle state of a WorldwideDay (status, type, period timestamps, VWAP).",
    { worldwide_day: wwd },
    handler(async ({ worldwide_day }) =>
      ok(await view(ctx, "metadosis", "getWorldwideDay", [worldwide_day])),
    ),
  );

  // --- Oracle ----------------------------------------------------------------
  server.tool(
    "oracle_pairs",
    "All oracle price pairs (pairId, base, quote, active).",
    {},
    handler(async () => ok(await view(ctx, "oracle", "getPairs", []))),
  );

  server.tool(
    "oracle_rate",
    "Latest exchange rate for a base/quote pair.",
    { base: z.string(), quote: z.string() },
    handler(async ({ base, quote }) =>
      ok(await view(ctx, "oracle", "getExchangeRate", [base, quote])),
    ),
  );

  server.tool(
    "oracle_vwap",
    "VWAP for a base/quote pair. With lookback_seconds uses the rolling window; otherwise the day VWAP.",
    {
      base: z.string(),
      quote: z.string(),
      lookback_seconds: z.number().int().optional(),
    },
    handler(async ({ base, quote, lookback_seconds }) =>
      ok(
        lookback_seconds === undefined
          ? await view(ctx, "oracle", "getDayVwap", [base, quote])
          : await view(ctx, "oracle", "getVwap", [base, quote, lookback_seconds]),
      ),
    ),
  );

  // --- Validators ------------------------------------------------------------
  server.tool(
    "validators",
    "Active validator set + counts and current epoch.",
    {},
    handler(async () => {
      const [active, count, epoch] = await Promise.all([
        view(ctx, "validatorset", "getActiveValidators", []),
        view(ctx, "validatorset", "activeValidatorCount", []),
        view(ctx, "validatorset", "getEpochNumber", []),
      ]);
      return ok({ active, activeCount: count, epoch });
    }),
  );

  server.tool(
    "validator",
    "Full validator record by address (stake, status, miss counters, epoch heights).",
    { address: addr },
    handler(async ({ address }) =>
      ok(await view(ctx, "validatorset", "validatorByAddress", [address])),
    ),
  );

  server.tool(
    "staking_info",
    "Stake delegated to a validator and total staked.",
    { validator: addr },
    handler(async ({ validator }) => {
      const [stake, total] = await Promise.all([
        view(ctx, "staking", "getStake", [validator]),
        view(ctx, "staking", "getTotalStaked", []),
      ]);
      return ok({ validator, stake, total });
    }),
  );

  server.tool(
    "rewards_pending",
    "Pending validator rewards for an address (in COEN).",
    { validator: addr },
    handler(async ({ validator }) => ok(await view(ctx, "rewards", "pendingRewards", [validator]))),
  );
}
