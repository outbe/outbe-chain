import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { z } from "zod";
import { type Address, type Hex, getAddress } from "viem";
import { FIELD, openGratis, openPromis, plain, signerAmounts } from "../confidential/balances.js";
import { signer } from "../confidential/authorize.js";
import { fidelityQueryAuth } from "../confidential/fidelity.js";
import { type EncryptedNod, signerNodAmount } from "../confidential/nod.js";
import { contextNetwork } from "../net/resolver.js";
import { address } from "./schemas.js";
import { listProposals } from "../governance.js";
import { pairTable } from "../oracle/pairs.js";
import { type Ctx, readView } from "../chain.js";
import { CONTRACTS, OFFERING_STATUS, PROPOSAL_STATUS } from "../registry.js";
import { ownedTokenIds } from "../read.js";
import { handler, ok, view } from "./util.js";

const wwd = z.number().int().describe("WorldwideDay as YYYYMMDD, e.g. 20260601");

function registerEntityViews(server: McpServer, ctx: Ctx): void {
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
    { id: z.string().describe("Tribute token id as a uint256 (decimal or 0x hex)") },
    handler(async ({ id }) => {
      const [metadata, owner] = await Promise.all([
        view(ctx, "tribute", "tokenURI", [id]),
        view(ctx, "tribute", "ownerOf", [id]),
      ]);
      return ok({ tokenId: id, owner, metadata });
    }),
  );

  server.tool(
    "tributes_by_owner",
    "List Tribute token ids owned by an address.",
    { owner: address },
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
    "Nod NFT data by token id (decoded) plus parsed tokenURI metadata; for the signer's own Nod also its " +
      "Gratis amount, opened with the creator key.",
    { id: z.string().describe("Nod token id as a uint256 (decimal or 0x hex)") },
    handler(async ({ id }) => {
      const [data, metadata, gratisAmount] = await Promise.all([
        view(ctx, "nod", "getNod", [id]),
        view(ctx, "nod", "tokenURI", [id]),
        signerNodGratis(ctx, BigInt(id)),
      ]);
      return ok({ nodId: id, data, metadata, ...(gratisAmount && { gratisAmount }) });
    }),
  );

  server.tool(
    "nods_by_owner",
    "List Nod token ids owned by an address (Nod has no bulk getter, so this " +
      "enumerates balanceOf -> tokenOfOwnerByIndex).",
    { owner: address },
    handler(async ({ owner }) => {
      const ids = await ownedTokenIds(ctx, "nod", owner);
      return ok({ owner, count: ids.length, nodIds: ids.map(String) });
    }),
  );

}

const FIDELITY_AUTH_SECONDS = 300;

/** The Gratis amount of the signer's own Nod; undefined for another owner or a non-Outbe network. */
async function signerNodGratis(ctx: Ctx, nodId: bigint): Promise<Record<string, unknown> | undefined> {
  const network = contextNetwork(ctx);
  if (!ctx.account || !network.isOutbe) return undefined;
  const { result } = await readView(ctx, CONTRACTS.nod, "getNod", [nodId]);
  const nod = result as EncryptedNod;
  if (getAddress(nod.owner) !== ctx.account.address) return undefined;
  return signerNodAmount(ctx, network, nod).then(plain, (error: Error) => ({ decryptError: error.message }));
}

function registerPositionViews(server: McpServer, ctx: Ctx): void {
  // --- Gem -------------------------------------------------------------------
  server.tool(
    "gem_get",
    "Gem NFT status by token id (decoded: type, state, load, prices, currency, issued), whether it has " +
      "qualified (born so, or derived from finalized daily VWAPs) plus parsed tokenURI metadata.",
    { id: z.string().describe("Gem token id (decimal or 0x hex)") },
    handler(async ({ id }) => {
      const [data, qualified, metadata] = await Promise.all([
        view(ctx, "gem", "getGem", [BigInt(id)]),
        view(ctx, "gem", "isQualified", [BigInt(id)]),
        view(ctx, "gem", "tokenURI", [BigInt(id)]),
      ]);
      return ok({ gemId: id, data, qualified, metadata });
    }),
  );

  server.tool(
    "gem_position_get",
    "Gem Factory position terms by id: merchant, source series, remaining capacity, " +
      "the entry/floor prices its gems are anchored to, both currencies and when it was opened.",
    { id: z.string().describe("Position token id (decimal or 0x hex)") },
    handler(async ({ id }) =>
      ok(await view(ctx, "gemfactory", "getPosition", [BigInt(id)])),
    ),
  );

  server.tool(
    "gem_settle_quote",
    "What settling a gem with a given asset costs, and which of the gem's two currencies " +
      "that asset settles on. `paymentMinor` is in the asset's own minor units; `snapshotId` " +
      "is the trailing VWAP snapshot an issuance-currency settlement must name (zero on " +
      "the reference rail). An issuance-currency quote expires at the next whole UTC hour.",
    {
      id: z.string().describe("Gem token id (decimal or 0x hex)"),
      asset: address.describe("Settlement stablecoin the holder intends to pay with"),
    },
    handler(async ({ id, asset }) =>
      ok(await view(ctx, "gemfactory", "quoteSettlement", [BigInt(id), asset])),
    ),
  );

  server.tool(
    "gems_by_owner",
    "List Gems owned by an address with decoded status for each (Gem has no bulk getter, " +
      "so this enumerates balanceOf -> tokenOfOwnerByIndex -> getGem).",
    { owner: address },
    handler(async ({ owner }) => {
      const ids = await ownedTokenIds(ctx, "gem", owner);
      const gems = await Promise.all(ids.map((id) => view(ctx, "gem", "getGem", [id])));
      return ok({ owner, count: ids.length, gems });
    }),
  );

  // --- Credis ----------------------------------------------------------------
  server.tool(
    "credis_get",
    "Credis by id (decoded: principal, outstanding, Gratis, prices, state) " +
      "plus parsed tokenURI metadata.",
    { id: z.string().describe("Credis id (decimal or 0x hex)") },
    handler(async ({ id }) => {
      const [data, metadata] = await Promise.all([
        view(ctx, "credis", "getCredis", [BigInt(id)]),
        view(ctx, "credis", "tokenURI", [BigInt(id)]),
      ]);
      return ok({ credisId: id, data, metadata });
    }),
  );

  server.tool(
    "credis_by_owner",
    "Credis an address owns, decoded like credis_get without metadata.",
    { owner: address },
    handler(async ({ owner }) => {
      const ids = await ownedTokenIds(ctx, "credis", owner);
      const credis = await Promise.all(ids.map((id) => view(ctx, "credis", "getCredis", [id])));
      return ok({ owner, count: credis.length, credis });
    }),
  );

  // --- Balances --------------------------------------------------------------
  server.tool(
    "gratis_balance",
    "Encrypted Gratis liquid and pledged balances. For the signer's own account both are also shown " +
      "decrypted with its Gratis view key.",
    { account: address },
    handler(async ({ account }) => {
      const balance = await view(ctx, "gratis", "balanceOf", [account]);
      const pledged = await view(ctx, "gratis", "pledgedOf", [account]);
      const decrypted = await signerAmounts(ctx, contextNetwork(ctx), "Gratis", account as Address, (key) => ({
        balance: openGratis(key, account as Address, FIELD.balance, balance as Hex),
        pledged: openGratis(key, account as Address, FIELD.pledged, pledged as Hex),
      }));
      return ok({ account, balance, pledged, ...(decrypted && { decrypted }) });
    }),
  );

  server.tool(
    "promis_balance",
    "Encrypted Promis balance. For the signer's own account it is also shown decrypted with its Promis view key.",
    { account: address },
    handler(async ({ account }) => {
      const balance = await view(ctx, "promis", "balanceOf", [account]);
      const decrypted = await signerAmounts(ctx, contextNetwork(ctx), "Promis", account as Address, (key) => ({
        balance: openPromis(key, account as Address, balance as Hex),
      }));
      return ok({ account, balance, ...(decrypted && { decrypted }) });
    }),
  );

  server.tool(
    "fidelity_index",
    "The signer's own Fidelity Index, now or at a Unix `timestamp`. The cohort ledger is encrypted, so the read " +
      "carries an owner authorization signed here that expires in five minutes. Requires OUTBE_PRIVATE_KEY.",
    { timestamp: z.number().int().optional().describe("Unix timestamp (seconds); default now") },
    handler(async ({ timestamp }) => {
      const account = signer(ctx);
      const expiry = BigInt(Math.floor(Date.now() / 1000) + FIDELITY_AUTH_SECONDS);
      const auth = await fidelityQueryAuth(account, contextNetwork(ctx).chainId, expiry);
      const index =
        timestamp === undefined
          ? await view(ctx, "fidelity", "getFidelityIndex", [account.address, expiry, auth])
          : await view(ctx, "fidelity", "getFidelityIndexAt", [account.address, timestamp, expiry, auth]);
      return ok({ account: account.address, ...(timestamp !== undefined && { timestamp }), index });
    }),
  );

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
    "Claimable AgentReward balance for an account (in COEN), all three pools summed.",
    { account: address },
    handler(async ({ account }) =>
      ok(await view(ctx, "agentreward", "getClaimableBalance", [account])),
    ),
  );

  server.tool(
    "agentreward_pool_claimable",
    "Claimable AgentReward balance for an account in one pool (0 = WAA, 1 = SRA, 2 = CCA), in COEN.",
    { account: address, pool: z.number().int().min(0).max(2).describe("0 = WAA, 1 = SRA, 2 = CCA") },
    handler(async ({ account, pool }) =>
      ok(await view(ctx, "agentreward", "getPoolClaimableBalance", [account, pool])),
    ),
  );

}

function registerMarketViews(server: McpServer, ctx: Ctx): void {
  // --- Metadosis / WorldwideDay ---------------------------------------------
  server.tool(
    "worldwide_days_offering",
    "WorldwideDays currently in OFFERING status (the days a tribute offer can target).",
    {},
    handler(async () => {
      const wwds = (await view(ctx, "metadosis", "getWorldwideDaysByStatus", [OFFERING_STATUS])) as {
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
    "All oracle price pairs (index, base, quote, the decimals each side is quoted in, active).",
    {},
    handler(async () => {
      const pairs = await pairTable(ctx);
      return ok(pairs);
    }),
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

}

function registerValidatorViews(server: McpServer, ctx: Ctx): void {
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
    { address },
    handler(async ({ address }) =>
      ok(await view(ctx, "validatorset", "validatorByAddress", [address])),
    ),
  );

  server.tool(
    "staking_info",
    "Stake delegated to a validator and total staked.",
    { validator: address },
    handler(async ({ validator }) => {
      const [stake, total] = await Promise.all([
        view(ctx, "staking", "getStake", [validator]),
        view(ctx, "staking", "getTotalStaked", []),
      ]);
      return ok({ validator, stake, total });
    }),
  );

}

function registerGovernanceViews(server: McpServer, ctx: Ctx): void {
  // --- Governance (canon, meta-canon, OIP, GIP) - read-only -----------------
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
      ok(await view(ctx, "governance", "getOip", [BigInt(id)])),
    ),
  );

  server.tool(
    "gip_get",
    "One Governance Improvement Proposal by id: author, status, blocks, text hash, and full text.",
    { id: proposalId },
    handler(async ({ id }) =>
      ok(await view(ctx, "governance", "getGip", [BigInt(id)])),
    ),
  );

  // Index-backed, PAGINATED listing (metadata only - omits the full text).
  // The caller must give exactly one of `author` / `status`. Each maps to a
  // dedicated on-chain index (get*ByAuthor / get*ByStatus). Returns `total` (the
  // whole bucket size) plus the requested `[offset, offset+limit)` page.
  const listFilter = {
    author: z.string().optional().describe("0x address - list this author's proposals"),
    status: z
      .enum(PROPOSAL_STATUS)
      .optional()
      .describe(`proposal status: ${PROPOSAL_STATUS.join(" | ")}`),
    offset: z.number().int().min(0).optional().describe("page start (default 0)"),
    limit: z.number().int().min(1).max(1000).optional().describe("page size (default 100, max 1000)"),
  };

  server.tool(
    "oip_list",
    "List OIPs by index, paginated (metadata only - omits the full text). Give `author` " +
      "(their OIPs) or `status` (a proposal status name), plus optional `offset`/`limit`.",
    listFilter,
    handler(async (args) => {
      const { total, offset, limit, items } = await listProposals(ctx, "Oip", args);
      return ok({ total, offset, limit, oips: items });
    }),
  );

  server.tool(
    "gip_list",
    "List GIPs by index, paginated (metadata only - omits the full text). Give `author` " +
      "(their GIPs) or `status` (a proposal status name), plus optional `offset`/`limit`.",
    listFilter,
    handler(async (args) => {
      const { total, offset, limit, items } = await listProposals(ctx, "Gip", args);
      return ok({ total, offset, limit, gips: items });
    }),
  );
}

export function registerViewTools(server: McpServer, ctx: Ctx): void {
  registerEntityViews(server, ctx);
  registerPositionViews(server, ctx);
  registerMarketViews(server, ctx);
  registerValidatorViews(server, ctx);
  registerGovernanceViews(server, ctx);
}
