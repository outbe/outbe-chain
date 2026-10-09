import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { type Hex, formatUnits } from "viem";
import { OUTBE_NETWORK } from "../../net/chains.js";
import { networkName } from "../schemas.js";
import { handler, ok } from "../util.js";
import { FACTORY_ABI, INTEX_ABI } from "../../intex/registry.js";
import { epochIso, intexState, fromSeriesId } from "../../intex/format.js";
import { addr, seriesMetadata, seriesQualified } from "../../intex/reads.js";
import { seriesArg } from "./args.js";
import type { IntexDeps } from "./deps.js";

/** The series ledger on outbe. */
export function registerSeriesTools(server: McpServer, deps: IntexDeps): void {
  const { resolveNetwork } = deps;
  server.tool(
    "intex_series_info",
    "Canonical series record from the outbe Intex: promis load, entry/floor/call prices, currencies, " +
      "lifecycle state (Issued/Qualified/Called/Expired), whether it has qualified (derived from finalized daily " +
      "VWAPs, never stored), issued/called timestamps, the settlementDeadline and the derived `expired` " +
      "flag - check `expired` before attempting settle (past-deadline settles revert) - " +
      "and how the issued units split into active, settled, exercised, sent to the Gem Factory and forfeited.",
    { series: seriesArg, network: networkName.optional() },
    handler(async ({ series, network }) => {
      const n = await resolveNetwork(network ?? OUTBE_NETWORK);
      const d = (await n.client.readContract({
        address: addr(n, "intex"),
        abi: INTEX_ABI,
        functionName: "seriesData",
        args: [series],
      })) as Record<string, bigint | number>;
      // The engine owns the split. Exercised units are not on the series record.
      const counts = (await n.client.readContract({
        address: addr(n, "factory"),
        abi: FACTORY_ABI,
        functionName: "seriesUnitCounts",
        args: [series],
      })) as Record<string, number>;
      const u256 = (v: bigint | number) => v as bigint;
      const settlementDeadline = Number(d.settlementDeadline);
      const [metadata, qualified] = await Promise.all([seriesMetadata(n, series), seriesQualified(n, series)]);
      return ok({
        network: n.name,
        seriesId: fromSeriesId(d.seriesId as unknown as Hex),
        // scales per crates/core/intex/src/schema.rs (SeriesRecord):
        promisLoadMinor: { raw: d.promisLoadMinor.toString(), value: formatUnits(u256(d.promisLoadMinor), 6) },
        entryPriceMinor: { raw: d.entryPriceMinor.toString(), value: formatUnits(u256(d.entryPriceMinor), 6), scale: "1e6 ISO stable-unit" },
        floorPriceMinor: { raw: d.floorPriceMinor.toString(), value: formatUnits(u256(d.floorPriceMinor), 6), scale: "1e6 ISO stable-unit" },
        callPriceMinor: { raw: d.callPriceMinor.toString(), value: formatUnits(u256(d.callPriceMinor), 6), scale: "1e6 ISO stable-unit" },
        issuedUnits: Number(counts.issuedUnits),
        // Disjoint classes summing to issuedUnits. Active units lose their load to
        // the pool once the call window closes and they are forfeited.
        activeUnits: Number(counts.activeUnits),
        settledUnits: Number(counts.settledUnits),
        exercisedUnits: Number(counts.exercisedUnits),
        gemFactoryUnits: Number(counts.gemFactoryUnits),
        forfeitedUnits: Number(counts.forfeitedUnits),
        callWindow: Number(d.callWindow),
        callThreshold: Number(d.callThreshold),
        callNoticePeriod: Number(d.callNoticePeriod),
        issuanceCurrency: Number(d.issuanceCurrency), // ISO 4217 numeric
        referenceCurrency: Number(d.referenceCurrency),
        worldwideDay: Number(d.worldwideDay),
        state: intexState(d.state),
        qualified,
        issuedAt: epochIso(d.issuedAt),
        calledAt: epochIso(d.calledAt),
        settlementDeadline: epochIso(settlementDeadline),
        expired: settlementDeadline > 0 && Math.floor(Date.now() / 1000) > settlementDeadline,
        metadata,
      });
    }),
  );

  server.tool(
    "intex_series_list",
    "Enumerate series ids that exist in the outbe Intex (dense enumeration).",
    { network: networkName.optional() },
    handler(async ({ network }) => {
      const n = await resolveNetwork(network ?? OUTBE_NETWORK);
      const total = Number(
        (await n.client.readContract({
          address: addr(n, "intex"),
          abi: INTEX_ABI,
          functionName: "totalSeries",
        })) as bigint,
      );
      const ids: string[] = [];
      for (let i = 0; i < total; i++) {
        const id = (await n.client.readContract({
          address: addr(n, "intex"),
          abi: INTEX_ABI,
          functionName: "seriesAt",
          args: [BigInt(i)],
        })) as Hex;
        ids.push(fromSeriesId(id));
      }
      return ok({ network: n.name, total, seriesIds: ids });
    }),
  );
}
