import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import {
  type Address,
  type Chain,
  type Hex,
  type PublicClient,
  type WalletClient,
  getAddress,
} from "viem";
import { z } from "zod";
import { type Ctx, createCtx } from "../chain.js";
import { handler, ok } from "./util.js";
import {
  AUCTION_ABI,
  type IntexAddresses,
  NETWORKS,
  NFT_ABI,
  REGISTRY_ABI,
  intexAddress,
} from "../intex/registry.js";
import { auctionStage, epochIso, intexState, intexStatus, isActiveStage } from "../intex/format.js";

/**
 * Intex participant tools: auction commit/reveal, escrow funding, NFT holdings,
 * the series ledger, the BSC->outbe bridge, and settlement/Promis on outbe.
 *
 * Domain (addresses, ABIs, decoders) lives in src/intex/. Networks come from the
 * NETWORKS table; a resolved network reuses the connected `ctx` when chain ids
 * match, else opens a fresh client via createCtx — same shape as src/tools/intent.ts.
 *
 * This file currently registers the read-only surface; signing tools land next.
 */

interface Network {
  name: string;
  chainId: number;
  chain: Chain;
  client: PublicClient;
  wallet?: WalletClient;
}

export function registerIntexTools(server: McpServer, ctx: Ctx): void {
  const pk = process.env.OUTBE_PRIVATE_KEY;
  const netCache = new Map<string, Network>();

  async function resolveNetwork(spec: string): Promise<Network> {
    const s = spec.trim().toLowerCase();
    const def = NETWORKS.find((d) => d.name.toLowerCase() === s || String(d.chainId) === s);
    if (!def) {
      throw new Error(`unknown network "${spec}"; supported: ${NETWORKS.map((d) => d.name).join(", ")}`);
    }
    const cached = netCache.get(def.name);
    if (cached) return cached;
    const c = def.chainId === ctx.chain.id ? ctx : await createCtx(def.rpc, pk);
    const n: Network = {
      name: def.name,
      chainId: c.chain.id,
      chain: c.chain,
      client: c.publicClient,
      wallet: c.walletClient,
    };
    netCache.set(def.name, n);
    return n;
  }

  /** The address arg or the configured signer; throws if neither is available. */
  function whoever(explicit?: string): Address {
    if (explicit) return getAddress(explicit);
    if (ctx.account) return ctx.account.address;
    throw new Error("no address given and no signer configured — pass an explicit address");
  }

  function addr(n: Network, key: keyof IntexAddresses): Address {
    return intexAddress(n.name, key);
  }

  const networkArg = z.string().describe(`network name (one of: ${NETWORKS.map((d) => d.name).join(", ")})`);
  const ownerArg = z.string().optional().describe("0x address (default: the configured signer)");

  // --- Series ledger (outbe IntexRegistry) -----------------------------------
  server.tool(
    "intex_series_info",
    "Canonical series record from the outbe IntexRegistry: size, strike, price floors, " +
      "lifecycle state (Issued/Qualified/Called), and issued/called timestamps.",
    { series: z.number().int().describe("series id"), network: networkArg.optional() },
    handler(async ({ series, network }) => {
      const n = await resolveNetwork(network ?? "outbe-testnet");
      const d = (await n.client.readContract({
        address: addr(n, "registry"),
        abi: REGISTRY_ABI,
        functionName: "seriesData",
        args: [series],
      })) as Record<string, bigint | number>;
      return ok({
        network: n.name,
        seriesId: Number(d.seriesId),
        intexSize: d.intexSize.toString(),
        intexStrikePrice: d.intexStrikePrice.toString(),
        coenPriceFloor: d.coenPriceFloor.toString(),
        coenPriceCallTrigger: d.coenPriceCallTrigger.toString(),
        issuedIntexCount: Number(d.issuedIntexCount),
        callWindowDays: Number(d.callWindowDays),
        callThresholdDays: Number(d.callThresholdDays),
        intexCallPeriod: Number(d.intexCallPeriod),
        state: intexState(d.state),
        issuedAt: epochIso(d.issuedAt),
        calledAt: epochIso(d.calledAt),
      });
    }),
  );

  server.tool(
    "intex_series_list",
    "Enumerate series ids that exist in the outbe IntexRegistry (dense enumeration).",
    { network: networkArg.optional() },
    handler(async ({ network }) => {
      const n = await resolveNetwork(network ?? "outbe-testnet");
      const total = Number(
        (await n.client.readContract({
          address: addr(n, "registry"),
          abi: REGISTRY_ABI,
          functionName: "totalSeries",
        })) as bigint,
      );
      const ids: number[] = [];
      for (let i = 0; i < total; i++) {
        const id = (await n.client.readContract({
          address: addr(n, "registry"),
          abi: REGISTRY_ABI,
          functionName: "seriesAt",
          args: [BigInt(i)],
        })) as number;
        ids.push(Number(id));
      }
      return ok({ network: n.name, total, seriesIds: ids });
    }),
  );

  // --- NFT holdings (BSC or outbe IntexNFT1155) ------------------------------
  server.tool(
    "intex_my_holdings",
    "Intex NFT holdings for an address: owned token ids, balances, and decoded status " +
      "(Issued/Settled). Defaults to bsc-testnet (where won NFTs land); pass network to read outbe.",
    { owner: ownerArg, network: networkArg.optional() },
    handler(async ({ owner, network }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const who = whoever(owner);
      const [tokenIds, balances] = (await n.client.readContract({
        address: addr(n, "nft"),
        abi: NFT_ABI,
        functionName: "getOwnedSeriesWithBalances",
        args: [who],
      })) as [bigint[], bigint[]];
      const holdings = await Promise.all(
        tokenIds.map(async (tokenId, i) => {
          const status = (await n.client.readContract({
            address: addr(n, "nft"),
            abi: NFT_ABI,
            functionName: "statusOf",
            args: [tokenId],
          })) as number;
          return { tokenId: tokenId.toString(), balance: balances[i].toString(), status: intexStatus(status) };
        }),
      );
      return ok({ network: n.name, owner: who, count: holdings.length, holdings });
    }),
  );

  server.tool(
    "intex_series_balance",
    "An address's Intex NFT balance for one series, split into issued and settled token ids.",
    { series: z.number().int().describe("series id"), owner: ownerArg, network: networkArg.optional() },
    handler(async ({ series, owner, network }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const who = whoever(owner);
      const [issued, settled] = (await n.client.readContract({
        address: addr(n, "nft"),
        abi: NFT_ABI,
        functionName: "tokenIds",
        args: [series],
      })) as [bigint, bigint];
      const [issuedBal, settledBal] = (await Promise.all([
        n.client.readContract({ address: addr(n, "nft"), abi: NFT_ABI, functionName: "balanceOf", args: [who, issued] }),
        n.client.readContract({ address: addr(n, "nft"), abi: NFT_ABI, functionName: "balanceOf", args: [who, settled] }),
      ])) as [bigint, bigint];
      return ok({
        network: n.name,
        series,
        owner: who,
        issued: { tokenId: issued.toString(), balance: issuedBal.toString() },
        settled: { tokenId: settled.toString(), balance: settledBal.toString() },
      });
    }),
  );

  // --- Auctions (BSC IntexAuction) -------------------------------------------
  const auctionStageOf = (n: Network, series: number) =>
    n.client.readContract({
      address: addr(n, "auction"),
      abi: AUCTION_ABI,
      functionName: "getAuctionStage",
      args: [series],
    }) as Promise<number>;

  server.tool(
    "intex_active_auctions",
    "Discover Intex auctions and their current stage. Finds series via AuctionStageUpdated logs, " +
      "then reads getAuctionStage for each. Returns only active stages (CommittingBids/RevealingBids) " +
      "unless include_all is set.",
    { network: networkArg.optional(), include_all: z.boolean().optional() },
    handler(async ({ network, include_all }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const logs = await n.client.getLogs({
        address: addr(n, "auction"),
        event: (AUCTION_ABI.find((x) => x.type === "event" && x.name === "AuctionStageUpdated") as never),
        fromBlock: 0n,
        toBlock: "latest",
      });
      const seriesIds = [
        ...new Set(logs.map((l) => Number((l as { args: { seriesId: number } }).args.seriesId))),
      ].sort((x, y) => x - y);
      const auctions = await Promise.all(
        seriesIds.map(async (series) => ({ series, stage: auctionStage(await auctionStageOf(n, series)) })),
      );
      const filtered = include_all ? auctions : auctions.filter((au) => isActiveStage(au.stage.code));
      return ok({ network: n.name, count: filtered.length, auctions: filtered });
    }),
  );

  server.tool(
    "intex_auction_info",
    "Full auction detail for one series: current stage plus schedule (commit/reveal/issuance ends), " +
      "params (sizes, min bid price/quantity, strike, floor) and cleared result.",
    { series: z.number().int().describe("series id"), network: networkArg.optional() },
    handler(async ({ series, network }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const [stage, info] = await Promise.all([
        auctionStageOf(n, series),
        n.client.readContract({ address: addr(n, "auction"), abi: AUCTION_ABI, functionName: "getAuctionInfo", args: [series] }),
      ]);
      const d = info as {
        worldwideDayState: number;
        schedule: { commitEnd: number; revealEnd: number; issuanceEnd: number };
        params: { intexSize: bigint; minIntexBidPrice: bigint; intexStrikePrice: bigint; coenPriceFloor: bigint; minIntexBidQuantity: number };
        result: { issuedIntexLoadedPromis: bigint; auctionIntexClearingPrice: bigint; issuedIntexCount: number; wonBidsCount: number };
      };
      return ok({
        network: n.name,
        series,
        stage: auctionStage(stage),
        worldwideDayState: d.worldwideDayState,
        schedule: {
          commitEnd: epochIso(d.schedule.commitEnd),
          revealEnd: epochIso(d.schedule.revealEnd),
          issuanceEnd: epochIso(d.schedule.issuanceEnd),
        },
        params: {
          intexSize: d.params.intexSize.toString(),
          minIntexBidPrice: d.params.minIntexBidPrice.toString(),
          intexStrikePrice: d.params.intexStrikePrice.toString(),
          coenPriceFloor: d.params.coenPriceFloor.toString(),
          minIntexBidQuantity: Number(d.params.minIntexBidQuantity),
        },
        result: {
          issuedIntexLoadedPromis: d.result.issuedIntexLoadedPromis.toString(),
          auctionIntexClearingPrice: d.result.auctionIntexClearingPrice.toString(),
          issuedIntexCount: Number(d.result.issuedIntexCount),
          wonBidsCount: Number(d.result.wonBidsCount),
        },
      });
    }),
  );

  server.tool(
    "intex_my_bids",
    "Your commit/reveal status across active auctions: for each active series, whether you have a " +
      "committed bid and whether it has been revealed. Pass series to check just one.",
    { owner: ownerArg, series: z.number().int().optional(), network: networkArg.optional() },
    handler(async ({ owner, series, network }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const who = whoever(owner);
      let targets: number[];
      if (series !== undefined) {
        targets = [series];
      } else {
        const logs = await n.client.getLogs({
          address: addr(n, "auction"),
          event: (AUCTION_ABI.find((x) => x.type === "event" && x.name === "AuctionStageUpdated") as never),
          fromBlock: 0n,
          toBlock: "latest",
        });
        const all = [...new Set(logs.map((l) => Number((l as { args: { seriesId: number } }).args.seriesId)))];
        const stages = await Promise.all(all.map(async (s) => ({ s, stage: await auctionStageOf(n, s) })));
        targets = stages.filter((x) => isActiveStage(x.stage)).map((x) => x.s).sort((x, y) => x - y);
      }
      const bids = await Promise.all(
        targets.map(async (s) => {
          const [commitHash, revealed] = (await Promise.all([
            n.client.readContract({ address: addr(n, "auction"), abi: AUCTION_ABI, functionName: "committedBidsByHash", args: [s, who] }),
            n.client.readContract({ address: addr(n, "auction"), abi: AUCTION_ABI, functionName: "revealedBidsByBidder", args: [s, who] }),
          ])) as [Hex, boolean];
          const committed = commitHash !== "0x" && /[1-9a-f]/i.test(commitHash.slice(2));
          return { series: s, committed, revealed, stage: auctionStage(await auctionStageOf(n, s)) };
        }),
      );
      const mine = bids.filter((b) => b.committed || b.revealed);
      return ok({ network: n.name, bidder: who, count: mine.length, bids: series !== undefined ? bids : mine });
    }),
  );

  // signing tools (commit/reveal, funding, bridge, settlement) register next.
}
