import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { formatUnits } from "viem";
import { z } from "zod";
import { OUTBE_NETWORK } from "../../net/chains.js";
import { networkName } from "../schemas.js";
import { handler, ok } from "../util.js";
import { AUCTION_ABI, DESIS_ABI, ORIGIN_ROUTER_ABI } from "../../intex/registry.js";
import { auctionStage, desisStage, epochIso, isActiveStage } from "../../intex/format.js";
import { priced } from "../../intex/units.js";
import { DEFAULT_DAYS_AHEAD, DEFAULT_DAYS_BACK, todayYmd, ymdShift } from "../../intex/dates.js";
import { bidDays, bidStatus } from "../../intex/bidder.js";
import { addr, auctionStageOf, discoverByDate } from "../../intex/reads.js";
import { accountArg, worldwideDayArg } from "./args.js";
import type { IntexDeps } from "./deps.js";

/** Auction discovery and per-chain fan-in. */
export function registerAuctionTools(server: McpServer, deps: IntexDeps): void {
  const { resolveNetwork, whoever, paymentMeta, target } = deps;
  server.tool(
    "auctions_active",
    "Active Intex auctions and their stage. Auction ids are worldwide days (yyyymmdd); probes a date window " +
      "(default today-30..+2, override via from_date/to_date). Active = CommittingBids or RevealingBids; " +
      "pass include_all for every stage.",
    {
      network: networkName.optional(),
      include_all: z.boolean().optional(),
      from_date: z.number().int().optional().describe("window start yyyymmdd (default today-30)"),
      to_date: z.number().int().optional().describe("window end yyyymmdd (default today+2)"),
    },
    handler(async ({ network, include_all, from_date, to_date }) => {
      const n = await target(network);
      const today = todayYmd();
      const from = from_date ?? ymdShift(today, -DEFAULT_DAYS_BACK);
      const to = to_date ?? ymdShift(today, DEFAULT_DAYS_AHEAD);
      const probed = await discoverByDate(n, from, to);
      const auctions = probed.map((p) => ({ worldwideDay: p.worldwideDay, stage: auctionStage(p.stage) }));
      const filtered = include_all ? auctions : auctions.filter((au) => isActiveStage(au.stage.code));
      return ok({ network: n.name, window: { from, to }, count: filtered.length, auctions: filtered });
    }),
  );

  server.tool(
    "auction_info",
    "One auction's stage, schedule (commit/reveal/issuance ends in UTC), and params (promis-load strike, " +
      "min bid rate/units, and one entry/floor/call row per currency the day prices - bid in one of those). " +
      "Bids are sealed: the bid counts and clearing result stay 0 until clearing runs after reveal, so 0 here " +
      "does NOT mean there are no participants.",
    { worldwideDay: worldwideDayArg, network: networkName.optional() },
    handler(async ({ worldwideDay, network }) => {
      const n = await target(network);
      const [stage, info, meta] = await Promise.all([
        auctionStageOf(n, worldwideDay),
        n.client.readContract({ address: addr(n, "auction"), abi: AUCTION_ABI, functionName: "getAuctionInfo", args: [worldwideDay] }),
        paymentMeta(n),
      ]);
      const dec = meta.decimals;
      const d = info;
      return ok({
        network: n.name,
        worldwideDay,
        stage: auctionStage(stage),
        worldwideDayState: d.worldwideDayState,
        schedule: {
          commitEnd: epochIso(d.schedule.commitEnd),
          revealEnd: epochIso(d.schedule.revealEnd),
          issuanceEnd: epochIso(d.schedule.issuanceEnd),
        },
        paymentToken: { symbol: meta.symbol, decimals: dec },
        params: {
          // Protocol-6 per-Intex PROMIS load. Escrow converts the calculated lock to WCOEN-18.
          promisLoadMinor: {
            raw: d.params.promisLoadMinor.toString(),
            value: formatUnits(d.params.promisLoadMinor, 6),
            scale: "1e6 PROMIS protocol units",
          },
          callTrigger: {
            callWindow: d.params.callTrigger.callWindow,
            callThreshold: d.params.callTrigger.callThreshold,
            callNoticePeriod: d.params.callTrigger.callNoticePeriod,
          },
          // bid rates are 1e6 fixed-point (fraction of strike).
          minIntexBidRate: { raw: d.params.minIntexBidRate.toString(), value: formatUnits(BigInt(d.params.minIntexBidRate), 6) },
          minIntexBidQuantity: Number(d.params.minIntexBidQuantity),
          // entry bond pulled at commit and returned at reveal/cancel. 0 = no bond.
          commitBondMinor: { raw: d.params.commitBondMinor.toString(), value: formatUnits(d.params.commitBondMinor, dec) },
          // A bid's reference currency must appear here.
          prices: d.params.prices.map((row) => ({
            isoCode: Number(row.isoCode),
            entryPriceMinor: priced(row.entryPriceMinor),
            floorPriceMinor: priced(row.floorPriceMinor),
            callPriceMinor: priced(row.callPriceMinor),
          })),
        },
        result: {
          note: "populated only after clearing",
          auctionClearingRate: { raw: d.result.auctionClearingRate.toString(), value: formatUnits(d.result.auctionClearingRate, 6) },
          wonBidsCount: Number(d.result.wonBidsCount),
          issuedUnits: Number(d.result.issuedUnits),
          issuedPromisLoadMinor: d.result.issuedPromisLoadMinor.toString(),
        },
      });
    }),
  );

  server.tool(
    "auction_chains",
    "Per-chain bid fan-in for one auction day, read from outbe: the day's target-chain snapshot and, for " +
      "each chain, whether its bids arrived in full (BIDS_DONE) and how many. Clearing runs once every " +
      "chain reports or the fan-in deadline passes; a chain still done=false after clearing was skipped " +
      "and its bidders reclaim locally (see auction_bids_by_owner on that chain).",
    { worldwideDay: worldwideDayArg, network: networkName.optional() },
    handler(async ({ worldwideDay, network }) => {
      const n = await resolveNetwork(network ?? OUTBE_NETWORK);
      const desis = addr(n, "desis");
      const chains = await n.client.readContract({
        address: addr(n, "originRouter"),
        abi: ORIGIN_ROUTER_ABI,
        functionName: "targetsOf",
        args: [worldwideDay],
      });
      const [stage, total] = await Promise.all([
        n.client.readContract({ address: desis, abi: DESIS_ABI, functionName: "getAuctionStage", args: [worldwideDay] }),
        n.client.readContract({ address: desis, abi: DESIS_ABI, functionName: "getBidsCount", args: [worldwideDay] }),
      ]);
      const perChain = await Promise.all(
        chains.map(async (chainId) => {
          const [done, bids] = await Promise.all([
            n.client.readContract({ address: desis, abi: DESIS_ABI, functionName: "isChainDone", args: [worldwideDay, chainId] }),
            n.client.readContract({ address: desis, abi: DESIS_ABI, functionName: "getChainBidsCount", args: [worldwideDay, chainId] }),
          ]);
          return { chainId, done, bids: Number(bids) };
        }),
      );
      return ok({
        network: n.name,
        worldwideDay,
        stage: desisStage(stage),
        totalBids: Number(total),
        chains: perChain,
      });
    }),
  );

  server.tool(
    "auction_bids_by_owner",
    "Your commit/reveal status across recent auctions, closed days included, plus your escrow money on that " +
      "chain: the commit bond (held from commit until reveal/cancel) and the bid lock (held from reveal until " +
      "you claim). Every bid ends in a claim - a winner's change, a loser's whole principal - so the lock " +
      "reports what auction_claim_refund pays and from when. Pass worldwideDay to check just one.",
    { account: accountArg, worldwideDay: worldwideDayArg.optional(), network: networkName.optional() },
    handler(async ({ account, worldwideDay, network }) => {
      const n = await target(network);
      const who = whoever(account);
      const targets = await bidDays(n, worldwideDay);
      const bids = await Promise.all(targets.map((wwd) => bidStatus(n, who, wwd)));
      const mine = bids.filter((b) => b.committed || b.revealed);
      return ok({ network: n.name, bidder: who, count: mine.length, bids: worldwideDay !== undefined ? bids : mine });
    }),
  );
}
