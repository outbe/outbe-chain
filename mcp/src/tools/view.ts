import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { z } from "zod";
import type { Ctx } from "../chain.js";
import { CONTRACTS, PROPOSAL_STATUS, proposalStatusCode, proposalStatusName } from "../registry.js";
import { handler, ok, view } from "./util.js";

const addr = z.string().describe("0x-prefixed address");
const wwd = z.number().int().describe("WorldwideDay as YYYYMMDD, e.g. 20260601");

/**
 * Tribute and Nod ids are 36-byte opaque entity ids (`bytes` in ITribute /
 * INod), not numeric ERC-721 token ids — take them as 0x hex verbatim.
 */
const entityId = (kind: string) =>
  z.string().describe(`${kind} id as 0x hex (36-byte entity id, e.g. from ${kind.toLowerCase()}s_by_owner)`);

function entityIdHex(id: string): `0x${string}` {
  if (!/^0x[0-9a-fA-F]*$/.test(id)) {
    throw new Error(`id must be 0x-prefixed hex (36-byte entity id), got "${id}"`);
  }
  return id as `0x${string}`;
}

/** Attach the human-readable proposal status name (statusCode -> {code, name}). */
function annotateProposal(p: unknown): Record<string, unknown> {
  const r = { ...(p as Record<string, unknown>) };
  const code = Number(r.statusCode);
  delete r.statusCode;
  return { ...r, status: { code, name: proposalStatusName(code) } };
}

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
    { id: entityId("Tribute") },
    handler(async ({ id }) => {
      const tributeId = entityIdHex(id);
      const [metadata, owner] = await Promise.all([
        view(ctx, "tribute", "tokenURI", [tributeId]),
        view(ctx, "tribute", "ownerOf", [tributeId]),
      ]);
      return ok({ tributeId, owner, metadata });
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
    "worldwide_day_totals",
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
    { id: entityId("Nod") },
    handler(async ({ id }) => {
      const nodId = entityIdHex(id);
      const [data, metadata] = await Promise.all([
        view(ctx, "nod", "nodData", [nodId]),
        view(ctx, "nod", "tokenURI", [nodId]),
      ]);
      return ok({ nodId, data, metadata });
    }),
  );

  server.tool(
    "nods_by_owner",
    "List Nod token ids owned by an address (Nod has no bulk getter, so this " +
      "enumerates balanceOf -> tokenOfOwnerByIndex).",
    { owner: addr },
    handler(async ({ owner }) => {
      const balance = Number(await view(ctx, "nod", "balanceOf", [owner]));
      const nodIds: unknown[] = [];
      for (let i = 0; i < balance; i++) {
        nodIds.push(await view(ctx, "nod", "tokenOfOwnerByIndex", [owner, i]));
      }
      return ok({ owner, count: balance, nodIds });
    }),
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

  // Per-account fidelity index is now encrypted and owner-signature-gated, so a
  // read-only MCP tool cannot fetch it. Expose the public synthetic-max ceiling
  // (`maxFidelityIndexAt`) instead — the upper bound on any account's RCFI at a
  // given time, in decayed days.
  server.tool(
    "fidelity_max_index",
    "Public synthetic-maximum Fidelity Index (saturating RCFI ceiling) at a Unix timestamp, in decayed days.",
    { timestamp: z.number().int().describe("Unix timestamp (seconds)") },
    handler(async ({ timestamp }) =>
      ok(await view(ctx, "fidelity", "maxFidelityIndexAt", [timestamp])),
    ),
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
    "worldwide_days_offering",
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
    "worldwide_day_get",
    "Full lifecycle state of a WorldwideDay (status, type, period timestamps, VWAP).",
    { worldwide_day: wwd },
    handler(async ({ worldwide_day }) =>
      ok(await view(ctx, "metadosis", "getWorldwideDay", [worldwide_day])),
    ),
  );

  // --- Oracle ----------------------------------------------------------------
  server.tool(
    "currency_pairs",
    "All oracle price pairs (pairId, base, quote, active).",
    {},
    handler(async () => ok(await view(ctx, "oracle", "getPairs", []))),
  );

  server.tool(
    "currency_rate",
    "Latest exchange rate for a base/quote pair.",
    { base: z.string(), quote: z.string() },
    handler(async ({ base, quote }) =>
      ok(await view(ctx, "oracle", "getExchangeRate", [base, quote])),
    ),
  );

  server.tool(
    "currency_rate_vwap",
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
    "validator_get",
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

  // No `rewards_claimable`: the Rewards precompile (0xEE03) exposes no callable
  // ABI. Validator daily emission is delivered as gems and per-block fees settle
  // internally in the LateFinalizeCredits begin-zone phase, so there is no
  // claimable native balance to read. See crates/system/rewards/src/precompile.rs.

  // --- Governance (canon, meta-canon, OIP, GIP) — read-only -----------------
  server.tool(
    "metacanon_get",
    "The meta-canon: current text + version + keccak hash (constitutional layer regulating the canon).",
    {},
    handler(async () => ok(await view(ctx, "governance", "getMetaCanon", []))),
  );

  server.tool(
    "canon_get",
    "The canon: current text + version + keccak hash (active protocol norms).",
    {},
    handler(async () => ok(await view(ctx, "governance", "getCanon", []))),
  );

  const proposalId = z.string().describe("Proposal id (decimal or 0x hex), 1-based");

  server.tool(
    "oip_get",
    "One Outbe Improvement Proposal by id: author, status, blocks, text hash, and full text.",
    { id: proposalId },
    handler(async ({ id }) =>
      ok(annotateProposal(await view(ctx, "governance", "getOip", [BigInt(id)]))),
    ),
  );

  server.tool(
    "gip_get",
    "One Governance Improvement Proposal by id: author, status, blocks, text hash, and full text.",
    { id: proposalId },
    handler(async ({ id }) =>
      ok(annotateProposal(await view(ctx, "governance", "getGip", [BigInt(id)]))),
    ),
  );

  // Index-backed, PAGINATED listing (metadata only — omits the full text).
  // Exactly one of `author` / `status` must be given; each maps to a dedicated
  // on-chain index (get*sByAuthor / get*sByStatus). Returns `total` (the whole
  // bucket size) plus the requested `[offset, offset+limit)` page.
  const listFilter = {
    author: z.string().optional().describe("0x address — list this author's proposals"),
    status: z
      .enum(PROPOSAL_STATUS)
      .optional()
      .describe(`proposal status: ${PROPOSAL_STATUS.join(" | ")}`),
    offset: z.number().int().min(0).optional().describe("page start (default 0)"),
    limit: z.number().int().min(1).max(1000).optional().describe("page size (default 100, max 1000)"),
  };

  async function listProposals(
    kind: "Oip" | "Gip",
    args: { author?: string; status?: string; offset?: number; limit?: number },
  ): Promise<{ total: number; offset: number; limit: number; items: unknown[] }> {
    const { author, status } = args;
    if ((author === undefined) === (status === undefined)) {
      throw new Error(
        `provide exactly one of \`author\` or \`status\` (${PROPOSAL_STATUS.join("|")})`,
      );
    }
    const offset = args.offset ?? 0;
    const limit = args.limit ?? 100;
    const by = author !== undefined ? "Author" : "Status";
    const key = author !== undefined ? author : proposalStatusCode(status as string);
    const listFn = `get${kind}sBy${by}`;
    const countFn = `${kind.toLowerCase()}CountBy${by}`;
    const [metas, total] = await Promise.all([
      view(ctx, "governance", listFn, [key, offset, limit]) as Promise<unknown[]>,
      view(ctx, "governance", countFn, [key]),
    ]);
    return { total: Number(total), offset, limit, items: metas.map(annotateProposal) };
  }

  server.tool(
    "oip_list",
    "List OIPs by index, paginated (metadata only — omits the full text). Give `author` " +
      `(their OIPs) or \`status\` = ${PROPOSAL_STATUS.join(" | ")}, plus optional \`offset\`/\`limit\`.`,
    listFilter,
    handler(async (args) => {
      const { total, offset, limit, items } = await listProposals("Oip", args);
      return ok({ total, offset, limit, oips: items });
    }),
  );

  server.tool(
    "gip_list",
    "List GIPs by index, paginated (metadata only — omits the full text). Give `author` " +
      `(their GIPs) or \`status\` = ${PROPOSAL_STATUS.join(" | ")}, plus optional \`offset\`/\`limit\`.`,
    listFilter,
    handler(async (args) => {
      const { total, offset, limit, items } = await listProposals("Gip", args);
      return ok({ total, offset, limit, gips: items });
    }),
  );
}
