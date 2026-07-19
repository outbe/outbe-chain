import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import {
  type Account,
  type Address,
  type Chain,
  type Hex,
  type PublicClient,
  type WalletClient,
  encodeFunctionData,
  formatUnits,
  getAddress,
  maxUint256,
  pad,
  parseAbiItem,
  parseUnits,
} from "viem";
import { z } from "zod";
import { type Ctx, createCtx } from "../chain.js";
import { handler, ok } from "./util.js";
import {
  AUCTION_ABI,
  DESIS_ABI,
  ERC20_ABI,
  ESCROW_ABI,
  FACTORY_ABI,
  type IntexAddresses,
  NETWORKS,
  NFT_ABI,
  NFT_BRIDGE_ABI,
  INTEX_ABI,
  ORIGIN_ROUTER_ABI,
  bridgeDstChainId,
  intexAddress,
} from "../intex/registry.js";
import { auctionStage, desisStage, epochIso, intexState, intexStatus, isActiveStage, lockStatus } from "../intex/format.js";
import { commitHash, revealBidTypedData } from "../intex/bid.js";
import { POW_DIFFICULTY, grindNonce } from "../intex/pow.js";

/**
 * Intex participant tools: auction commit/reveal, escrow funding, NFT holdings,
 * the series ledger, the BSC->outbe bridge, and settlement/Promis on outbe.
 *
 * Domain (addresses, ABIs, decoders) lives in src/intex/. Networks come from the
 * NETWORKS table; a resolved network reuses the connected `ctx` when chain ids
 * match, else opens a fresh client via createCtx — same shape as src/tools/intent.ts.
 *
 * Read tools work without a key; signing tools require OUTBE_PRIVATE_KEY.
 */

interface Network {
  name: string;
  chainId: number;
  chain: Chain;
  client: PublicClient;
  wallet?: WalletClient;
}

const PROMIS_MINED_EVENT = parseAbiItem(
  "event PromisMined(uint32 indexed seriesId, address indexed holder, uint256 amount, uint256 promisAmount)",
);

// Auction ids are worldwide days (yyyymmdd), one per day; the auction runs weeks
// after its day, so active ids sit up to ~26 days in the past. Discovery probes
// getAuctionStage across a date window — a few cheap point reads — rather than
// scanning logs, which public RPCs range-limit.
const DEFAULT_DAYS_BACK = 30;
const DEFAULT_DAYS_AHEAD = 2;
const DAY_MS = 86_400_000;

function ymdToDate(ymd: number): Date {
  return new Date(Date.UTC(Math.floor(ymd / 10000), (Math.floor(ymd / 100) % 100) - 1, ymd % 100));
}
function dateToYmd(dt: Date): number {
  return dt.getUTCFullYear() * 10000 + (dt.getUTCMonth() + 1) * 100 + dt.getUTCDate();
}
function todayYmd(): number {
  return dateToYmd(new Date());
}
function ymdShift(ymd: number, days: number): number {
  return dateToYmd(new Date(ymdToDate(ymd).getTime() + days * DAY_MS));
}
function ymdRange(from: number, to: number): number[] {
  const out: number[] = [];
  for (const dt = ymdToDate(from); dateToYmd(dt) <= to; dt.setUTCDate(dt.getUTCDate() + 1)) out.push(dateToYmd(dt));
  return out;
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

  function requireAccount(): Account {
    if (!ctx.account) {
      throw new Error("signing requires a key — set OUTBE_PRIVATE_KEY in the MCP server env");
    }
    return ctx.account;
  }

  async function estimateGas(n: Network, to: Address, data: Hex, value: bigint): Promise<bigint> {
    const est = await n.client.estimateGas({ account: ctx.account?.address, to, data, value });
    return (est * 130n) / 100n;
  }

  async function send(n: Network, to: Address, data: Hex, value: bigint, gas: bigint): Promise<Hex> {
    const account = requireAccount();
    if (!n.wallet) throw new Error(`no signer for ${n.name}`);
    return n.wallet.sendTransaction({ account, chain: n.chain, to, data, value, gas });
  }

  /** Submit a tx and, unless wait===false, wait for and summarize its receipt. */
  async function submit(n: Network, to: Address, data: Hex, value: bigint, wait?: boolean) {
    const gas = await estimateGas(n, to, data, value);
    const hash = await send(n, to, data, value, gas);
    if (wait === false) return { txHash: hash, status: "submitted" as const };
    const r = await n.client.waitForTransactionReceipt({ hash, timeout: 180_000 });
    return { txHash: hash, status: r.status, blockNumber: r.blockNumber.toString(), gasUsed: r.gasUsed.toString() };
  }

  // A bid is a RATE: the fraction of the per-Intex strike (promis_load, in wCOEN)
  // the bidder will pay, as 1e6 fixed-point. Payment-token meta (wCOEN, 18 dec) is
  // cached per network so outputs can name the token and size the escrow lock.
  const RATE_SCALE = 1_000_000n;
  const metaCache = new Map<string, { decimals: number; symbol: string }>();
  async function paymentMeta(n: Network): Promise<{ decimals: number; symbol: string }> {
    const cached = metaCache.get(n.name);
    if (cached) return cached;
    const token = addr(n, "paymentToken");
    const [decimals, symbol] = (await Promise.all([
      n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "decimals" }),
      n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "symbol" }),
    ])) as [number, string];
    const meta = { decimals: Number(decimals), symbol };
    metaCache.set(n.name, meta);
    return meta;
  }
  /** Bid rate as a fraction of strike ("0.8" = 80%) to the uint32 1e6 fixed-point the contract expects. */
  function toBidRate(rate: string): bigint {
    const raw = parseUnits(rate, 6);
    if (raw < 0n || raw > RATE_SCALE) throw new Error(`bid rate ${rate} must be 0..1 (0-100% of strike)`);
    return raw;
  }

  // --- shared argument schemas ---
  const networkArg = z.string().describe(`network name (one of: ${NETWORKS.map((d) => d.name).join(", ")})`);
  const accountArg = z.string().optional().describe("0x address to query (default: the configured signer)");
  const seriesArg = z.number().int().describe("series id");
  const worldwideDayArg = z.number().int().describe("auction worldwide day (yyyymmdd)");
  const quantityArg = z.number().int().describe("bid quantity (uint16)");
  const rateArg = z
    .string()
    .describe('bid rate as a fraction of strike, 0..1 (e.g. "0.8" = 80% of strike; min from auction_info)');
  const amountArg = z.string().describe("amount as the raw on-chain integer");
  const recipientArg = z.string().optional().describe("recipient on outbe (default: the signer)");
  const waitArg = z.boolean().optional().describe("wait for the receipt (default true)");

  // --- Series ledger (outbe Intex) -----------------------------------
  server.tool(
    "intex_series_info",
    "Canonical series record from the outbe Intex: promis load, entry/floor/call prices, currencies, " +
      "lifecycle state (Issued/Qualified/Called), and issued/called timestamps.",
    { series: seriesArg, network: networkArg.optional() },
    handler(async ({ series, network }) => {
      const n = await resolveNetwork(network ?? "outbe-testnet");
      const d = (await n.client.readContract({
        address: addr(n, "intex"),
        abi: INTEX_ABI,
        functionName: "seriesData",
        args: [series],
      })) as Record<string, bigint | number>;
      const u256 = (v: bigint | number) => v as bigint;
      return ok({
        network: n.name,
        seriesId: Number(d.seriesId),
        // scales per crates/core/intex/src/schema.rs (SeriesRecord):
        promisLoad: { raw: d.promisLoadMinor.toString(), value: formatUnits(u256(d.promisLoadMinor), 18) }, // Promis per intex, 18 dec
        entryPrice: { raw: d.entryPriceMinor.toString(), value: formatUnits(u256(d.entryPriceMinor), 18), scale: "1e18 oracle (reference ccy)" },
        floorPrice: { raw: d.floorPriceMinor.toString(), value: formatUnits(u256(d.floorPriceMinor), 18), scale: "1e18 oracle" },
        callPrice: { raw: d.callPriceMinor.toString(), value: formatUnits(u256(d.callPriceMinor), 18), scale: "1e18 oracle" },
        issuedIntexCount: Number(d.issuedIntexCount),
        callWindowDays: Number(d.callWindowDays),
        callThresholdDays: Number(d.callThresholdDays),
        intexCallPeriod: Number(d.intexCallPeriod),
        issuanceCurrency: Number(d.issuanceCurrency), // ISO 4217 numeric
        referenceCurrency: Number(d.referenceCurrency),
        state: intexState(d.state),
        issuedAt: epochIso(d.issuedAt),
        calledAt: epochIso(d.calledAt),
      });
    }),
  );

  server.tool(
    "intex_series_list",
    "Enumerate series ids that exist in the outbe Intex (dense enumeration).",
    { network: networkArg.optional() },
    handler(async ({ network }) => {
      const n = await resolveNetwork(network ?? "outbe-testnet");
      const total = Number(
        (await n.client.readContract({
          address: addr(n, "intex"),
          abi: INTEX_ABI,
          functionName: "totalSeries",
        })) as bigint,
      );
      const ids: number[] = [];
      for (let i = 0; i < total; i++) {
        const id = (await n.client.readContract({
          address: addr(n, "intex"),
          abi: INTEX_ABI,
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
    "intex_holdings_by_owner",
    "Intex NFT holdings for an address: owned token ids, balances, and decoded status " +
      "(Issued/Settled). Defaults to bsc-testnet (where won NFTs land); pass network to read outbe.",
    { account: accountArg, network: networkArg.optional() },
    handler(async ({ account, network }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const who = whoever(account);
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
      return ok({ network: n.name, account: who, count: holdings.length, holdings });
    }),
  );

  server.tool(
    "intex_series_balance",
    "An address's Intex NFT balance for one series, split into issued and settled token ids.",
    { series: seriesArg, account: accountArg, network: networkArg.optional() },
    handler(async ({ series, account, network }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const who = whoever(account);
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
        account: who,
        issued: { tokenId: issued.toString(), balance: issuedBal.toString() },
        settled: { tokenId: settled.toString(), balance: settledBal.toString() },
      });
    }),
  );

  // --- Auctions (BSC IntexAuction) -------------------------------------------
  const auctionStageOf = (n: Network, worldwideDay: number) =>
    n.client.readContract({
      address: addr(n, "auction"),
      abi: AUCTION_ABI,
      functionName: "getAuctionStage",
      args: [worldwideDay],
    }) as Promise<number>;

  /** Probe getAuctionStage across a yyyymmdd date window; drop dates with no auction. */
  async function discoverByDate(n: Network, fromDate: number, toDate: number): Promise<{ worldwideDay: number; stage: number }[]> {
    const probed = await Promise.all(
      ymdRange(fromDate, toDate).map(async (worldwideDay) => {
        try {
          return { worldwideDay, stage: await auctionStageOf(n, worldwideDay) };
        } catch {
          return null; // getAuctionStage reverts AuctionNotFound for empty dates
        }
      }),
    );
    return probed.filter((x): x is { worldwideDay: number; stage: number } => x !== null);
  }

  server.tool(
    "auctions_active",
    "Active Intex auctions and their stage. Auction ids are worldwide days (yyyymmdd); probes a date window " +
      "(default today-30..+2, override via from_date/to_date). Active = CommittingBids or RevealingBids; " +
      "pass include_all for every stage.",
    {
      network: networkArg.optional(),
      include_all: z.boolean().optional(),
      from_date: z.number().int().optional().describe("window start yyyymmdd (default today-30)"),
      to_date: z.number().int().optional().describe("window end yyyymmdd (default today+2)"),
    },
    handler(async ({ network, include_all, from_date, to_date }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
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
      "min bid rate/quantity, entry/floor/call). Bids are sealed: the bid counts and clearing result stay 0 " +
      "until clearing runs after reveal, so 0 here does NOT mean there are no participants.",
    { worldwideDay: worldwideDayArg, network: networkArg.optional() },
    handler(async ({ worldwideDay, network }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const [stage, info, meta] = await Promise.all([
        auctionStageOf(n, worldwideDay),
        n.client.readContract({ address: addr(n, "auction"), abi: AUCTION_ABI, functionName: "getAuctionInfo", args: [worldwideDay] }),
        paymentMeta(n),
      ]);
      const dec = meta.decimals;
      const d = info as {
        worldwideDayState: number;
        schedule: { commitEnd: number; revealEnd: number; issuanceEnd: number };
        params: {
          issuanceCurrency: number;
          referenceCurrency: number;
          promisLoadMinor: bigint;
          callTrigger: { windowDays: number; thresholdDays: number; intexCallPeriod: number };
          minIntexBidRate: bigint;
          minIntexBidQuantity: number;
          entryPriceMinor: bigint;
          floorPriceMinor: bigint;
          callPriceMinor: bigint;
          commitBondMinor: bigint;
        };
        result: { auctionClearingRate: bigint; wonBidsCount: number; issuedIntexCount: number; issuedIntexLoadedPromis: bigint };
      };
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
          issuanceCurrency: d.params.issuanceCurrency,
          referenceCurrency: d.params.referenceCurrency,
          // strike basis: per-Intex promis_load in the payment token (wCOEN). Escrow lock = qty * this * rate / 1e6.
          promisLoadMinor: { raw: d.params.promisLoadMinor.toString(), value: formatUnits(d.params.promisLoadMinor, dec) },
          callTrigger: {
            windowDays: d.params.callTrigger.windowDays,
            thresholdDays: d.params.callTrigger.thresholdDays,
            intexCallPeriod: d.params.callTrigger.intexCallPeriod,
          },
          // bid rates are 1e6 fixed-point (fraction of strike).
          minIntexBidRate: { raw: d.params.minIntexBidRate.toString(), value: formatUnits(d.params.minIntexBidRate, 6) },
          minIntexBidQuantity: Number(d.params.minIntexBidQuantity),
          // entry bond pulled at commit and returned at reveal/cancel; 0 = no bond.
          commitBondMinor: { raw: d.params.commitBondMinor.toString(), value: formatUnits(d.params.commitBondMinor, dec) },
          // entry/floor/call are in the reference currency (USD); raw on-chain integers.
          entryPriceMinor: d.params.entryPriceMinor.toString(),
          floorPriceMinor: d.params.floorPriceMinor.toString(),
          callPriceMinor: d.params.callPriceMinor.toString(),
        },
        result: {
          note: "populated only after clearing",
          auctionClearingRate: { raw: d.result.auctionClearingRate.toString(), value: formatUnits(d.result.auctionClearingRate, 6) },
          wonBidsCount: Number(d.result.wonBidsCount),
          issuedIntexCount: Number(d.result.issuedIntexCount),
          issuedIntexLoadedPromis: d.result.issuedIntexLoadedPromis.toString(),
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
    { worldwideDay: worldwideDayArg, network: networkArg.optional() },
    handler(async ({ worldwideDay, network }) => {
      const n = await resolveNetwork(network ?? "outbe-testnet");
      const desis = addr(n, "desis");
      const chains = (await n.client.readContract({
        address: addr(n, "originRouter"),
        abi: ORIGIN_ROUTER_ABI,
        functionName: "targetsOf",
        args: [worldwideDay],
      })) as number[];
      const [stage, total] = (await Promise.all([
        n.client.readContract({ address: desis, abi: DESIS_ABI, functionName: "getAuctionStage", args: [worldwideDay] }),
        n.client.readContract({ address: desis, abi: DESIS_ABI, functionName: "getBidsCount", args: [worldwideDay] }),
      ])) as [number, bigint];
      const perChain = await Promise.all(
        chains.map(async (chainId) => {
          const [done, bids] = (await Promise.all([
            n.client.readContract({ address: desis, abi: DESIS_ABI, functionName: "isChainDone", args: [worldwideDay, chainId] }),
            n.client.readContract({ address: desis, abi: DESIS_ABI, functionName: "getChainBidsCount", args: [worldwideDay, chainId] }),
          ])) as [boolean, bigint];
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
    "Your commit/reveal status across active auctions, plus your escrow money on that chain: the commit " +
      "bond (held from commit until reveal/cancel) and the bid lock (held from reveal until finalization). " +
      "Emits a hint when funds are stuck — a no-reveal bond reclaimable via intex_claim_commit_bond, or a " +
      "never-finalized lock (e.g. the chain missed the clearing deadline) reclaimable in full via " +
      "auction_claim_refund after the shown refundClaimableAt. Pass worldwideDay to check just one.",
    { account: accountArg, worldwideDay: worldwideDayArg.optional(), network: networkArg.optional() },
    handler(async ({ account, worldwideDay, network }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const who = whoever(account);
      let targets: number[];
      if (worldwideDay !== undefined) {
        targets = [worldwideDay];
      } else {
        const today = todayYmd();
        const probed = await discoverByDate(n, ymdShift(today, -DEFAULT_DAYS_BACK), ymdShift(today, DEFAULT_DAYS_AHEAD));
        targets = probed.filter((x) => isActiveStage(x.stage)).map((x) => x.worldwideDay).sort((x, y) => x - y);
      }
      const refundDelay = Number(
        (await n.client.readContract({ address: addr(n, "escrow"), abi: ESCROW_ABI, functionName: "REFUND_DELAY" })) as number,
      );
      const bids = await Promise.all(
        targets.map(async (wwd) => {
          const [commitHash, revealed, lock, bond] = (await Promise.all([
            n.client.readContract({ address: addr(n, "auction"), abi: AUCTION_ABI, functionName: "committedBidsByHash", args: [wwd, who] }),
            n.client.readContract({ address: addr(n, "auction"), abi: AUCTION_ABI, functionName: "revealedBidsByBidder", args: [wwd, who] }),
            n.client.readContract({ address: addr(n, "escrow"), abi: ESCROW_ABI, functionName: "getBidLock", args: [wwd, who] }),
            n.client.readContract({ address: addr(n, "escrow"), abi: ESCROW_ABI, functionName: "getCommitBond", args: [wwd, who] }),
          ])) as [
            Hex,
            boolean,
            { lockedAmount: bigint; lockedAt: number; status: number; failedRefund: bigint; splitRecorded: boolean },
            { amount: bigint; lockedAt: number },
          ];
          const committed = commitHash !== "0x" && /[1-9a-f]/i.test(commitHash.slice(2));
          const stage = await auctionStageOf(n, wwd);
          const out: Record<string, unknown> = { worldwideDay: wwd, committed, revealed, stage: auctionStage(stage) };
          const hints: string[] = [];
          if (bond.amount > 0n) {
            out.commitBond = { amount: bond.amount.toString(), lockedAt: epochIso(bond.lockedAt) };
            // A held bond during commit/reveal is normal (it returns at reveal/cancel); past
            // that window a no-reveal commit left it behind.
            if (!revealed && !isActiveStage(stage)) {
              hints.push(
                "entry bond left by a no-reveal commit; reclaim via intex_claim_commit_bond (immediately on a cancelled day, else 21 days past revealEnd)",
              );
            }
          }
          if (lock.status !== 0) {
            const [, , , finalized] = (await n.client.readContract({
              address: addr(n, "escrow"),
              abi: ESCROW_ABI,
              functionName: "auctionEscrowState",
              args: [wwd],
            })) as [bigint, number, number, boolean];
            out.escrow = {
              lockedAmount: lock.lockedAmount.toString(),
              status: lockStatus(lock.status),
              finalized,
              refundClaimableAt: epochIso(lock.lockedAt + refundDelay),
            };
            // Locked + never finalized = no refund instructions reached this chain; the bidder
            // self-serves the full principal once the delay passes.
            if (lock.status === 1 && !finalized) {
              hints.push(
                "escrow not finalized on this chain; if no refund arrives, claim the full lock via auction_claim_refund from refundClaimableAt",
              );
            }
          }
          if (hints.length > 0) out.hints = hints;
          return out as { worldwideDay: number; committed: boolean; revealed: boolean };
        }),
      );
      const mine = bids.filter((b) => b.committed || b.revealed);
      return ok({ network: n.name, bidder: who, count: mine.length, bids: worldwideDay !== undefined ? bids : mine });
    }),
  );

  // --- Bid commit / reveal (BSC IntexAuction, signed) ------------------------
  async function signReveal(n: Network, account: Account, worldwideDay: number, quantity: number, bidRate: bigint): Promise<Hex> {
    const typedData = revealBidTypedData({
      chainId: n.chainId,
      verifyingContract: addr(n, "auction"),
      worldwideDay,
      bidder: account.address,
      quantity,
      bidRate: Number(bidRate),
    });
    if (!account.signTypedData) throw new Error("the configured account cannot sign typed data");
    return account.signTypedData(typedData);
  }

  server.tool(
    "auction_bid_commit",
    "Commit a sealed Intex bid: signs the EIP-712 RevealBid and submits keccak256(signature) as the commit " +
      "hash (no separate salt). When the auction carries an entry bond (commitBondMinor > 0), commitBid pulls " +
      "it into escrow in the same transaction — the tool auto-approves the escrow if the allowance is short. " +
      "The bond returns at reveal/cancel; a green-day no-reveal locks it for 21 days past revealEnd " +
      "(intex_claim_commit_bond). IMPORTANT: save your (worldwideDay, quantity, rate); you must repeat them to " +
      "reveal, they can't be recovered on-chain, and are only remembered this session. Requires OUTBE_PRIVATE_KEY.",
    { worldwideDay: worldwideDayArg, quantity: quantityArg, rate: rateArg, network: networkArg.optional(), wait: waitArg },
    handler(async ({ worldwideDay, quantity, rate, network, wait }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const account = requireAccount();
      const bidRate = toBidRate(rate);

      // Entry bond: the escrow pulls it inside commitBid, so cover the allowance up front.
      const info = (await n.client.readContract({
        address: addr(n, "auction"),
        abi: AUCTION_ABI,
        functionName: "getAuctionInfo",
        args: [worldwideDay],
      })) as { params: { commitBondMinor: bigint } };
      const bond = info.params.commitBondMinor;
      let autoApprove: { txHash: Hex; amount: string } | null = null;
      let note = "No entry bond on this worldwideDay; nothing is locked at commit.";
      if (bond > 0n) {
        const { decimals: dec, symbol } = await paymentMeta(n);
        const bondHuman = formatUnits(bond, dec);
        const token = addr(n, "paymentToken");
        const escrow = addr(n, "escrow");
        const allowance = (await n.client.readContract({
          address: token,
          abi: ERC20_ABI,
          functionName: "allowance",
          args: [account.address, escrow],
        })) as bigint;
        if (allowance < bond) {
          const approveData = encodeFunctionData({ abi: ERC20_ABI, functionName: "approve", args: [escrow, bond] });
          const ar = await submit(n, token, approveData, 0n, true); // must be mined before commit
          autoApprove = { txHash: ar.txHash, amount: bond.toString() };
        }
        note =
          `Commit locks a ${bondHuman} ${symbol} entry bond in escrow; it returns at reveal/cancel. ` +
          `A green-day no-reveal keeps it locked until 21 days past revealEnd (intex_claim_commit_bond).`;
      }

      const signature = await signReveal(n, account, worldwideDay, quantity, bidRate);
      const hash = commitHash(signature);
      const data = encodeFunctionData({ abi: AUCTION_ABI, functionName: "commitBid", args: [worldwideDay, hash] });
      const receipt = await submit(n, addr(n, "auction"), data, 0n, wait);
      return ok({
        network: n.name,
        worldwideDay,
        quantity,
        rate,
        bidRate: bidRate.toString(),
        commitHash: hash,
        bond: bond.toString(),
        autoApprove,
        note,
        ...receipt,
        reminder:
          `Record worldwideDay=${worldwideDay}, quantity=${quantity}, rate=${rate} — required to reveal, ` +
          `not recoverable on-chain, remembered only this session.`,
      });
    }),
  );

  server.tool(
    "auction_bid_reveal",
    "Reveal a committed Intex bid: re-derives the same signature from (worldwideDay, quantity, rate) and submits " +
      "revealBid; the escrow then locks quantity * strike * rate / RATE_SCALE in wCOEN, where strike is the " +
      "auction's promis_load. Auto-approves the escrow first if the allowance is short. Requires OUTBE_PRIVATE_KEY.",
    { worldwideDay: worldwideDayArg, quantity: quantityArg, rate: rateArg, network: networkArg.optional(), wait: waitArg },
    handler(async ({ worldwideDay, quantity, rate, network, wait }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const account = requireAccount();
      const { decimals: dec, symbol } = await paymentMeta(n);
      const bidRate = toBidRate(rate);

      // Escrow lock = quantity * strike * bidRate / RATE_SCALE, where strike is the auction's
      // per-Intex promisLoadMinor (wCOEN). Read it so the auto-approve covers exactly the lock.
      const info = (await n.client.readContract({
        address: addr(n, "auction"),
        abi: AUCTION_ABI,
        functionName: "getAuctionInfo",
        args: [worldwideDay],
      })) as { params: { promisLoadMinor: bigint; commitBondMinor: bigint } };
      const strike = info.params.promisLoadMinor;
      const lockAmount = (BigInt(quantity) * strike * bidRate) / RATE_SCALE;
      const lockHuman = formatUnits(lockAmount, dec);
      const token = addr(n, "paymentToken");
      const escrow = addr(n, "escrow");
      const allowance = (await n.client.readContract({
        address: token,
        abi: ERC20_ABI,
        functionName: "allowance",
        args: [account.address, escrow],
      })) as bigint;
      let autoApprove: { txHash: Hex; amount: string } | null = null;
      let note: string;
      if (allowance < lockAmount) {
        const approveData = encodeFunctionData({ abi: ERC20_ABI, functionName: "approve", args: [escrow, lockAmount] });
        const ar = await submit(n, token, approveData, 0n, true); // must be mined before reveal
        autoApprove = { txHash: ar.txHash, amount: lockAmount.toString() };
        note = `Reveal locks ${lockHuman} ${symbol} (${quantity} x strike x ${rate}) in escrow. Allowance was short, so the escrow was approved for ${lockHuman} ${symbol} first, then the bid was revealed.`;
      } else {
        note = `Reveal locks ${lockHuman} ${symbol} (${quantity} x strike x ${rate}) in escrow; allowance already covered it, no approval needed.`;
      }
      if (info.params.commitBondMinor > 0n) {
        note += ` The ${formatUnits(info.params.commitBondMinor, dec)} ${symbol} entry bond returns within the same transaction (released before the bid lock, so it can fund the bid).`;
      }

      const signature = await signReveal(n, account, worldwideDay, quantity, bidRate);
      const data = encodeFunctionData({
        abi: AUCTION_ABI,
        functionName: "revealBid",
        args: [worldwideDay, quantity, bidRate, BigInt(n.chainId), signature],
      });
      const receipt = await submit(n, addr(n, "auction"), data, 0n, wait);
      return ok({ network: n.name, worldwideDay, quantity, rate, bidRate: bidRate.toString(), locked: lockHuman, autoApprove, note, ...receipt });
    }),
  );

  server.tool(
    "auction_bid_cancel",
    "Cancel a committed bid for a worldwide day before the reveal stage. Requires OUTBE_PRIVATE_KEY.",
    { worldwideDay: worldwideDayArg, network: networkArg.optional(), wait: waitArg },
    handler(async ({ worldwideDay, network, wait }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      requireAccount();
      const data = encodeFunctionData({ abi: AUCTION_ABI, functionName: "cancelCommit", args: [worldwideDay] });
      const receipt = await submit(n, addr(n, "auction"), data, 0n, wait);
      return ok({ network: n.name, worldwideDay, ...receipt });
    }),
  );

  server.tool(
    "intex_claim_commit_bond",
    "Reclaim an entry bond left behind by a no-reveal commit. Permissionless and always pays the stored " +
      "bidder: a cancelled (red-day) auction releases immediately, otherwise the bond is claimable only " +
      "21 days past revealEnd. Requires OUTBE_PRIVATE_KEY.",
    { worldwideDay: worldwideDayArg, bidder: accountArg, network: networkArg.optional(), wait: waitArg },
    handler(async ({ worldwideDay, bidder, network, wait }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const account = requireAccount();
      const who = bidder ? getAddress(bidder) : account.address;
      const data = encodeFunctionData({ abi: AUCTION_ABI, functionName: "claimCommitBond", args: [worldwideDay, who] });
      const receipt = await submit(n, addr(n, "auction"), data, 0n, wait);
      return ok({ network: n.name, worldwideDay, bidder: who, ...receipt });
    }),
  );

  server.tool(
    "auction_claim_refund",
    "Reclaim a bid lock the finalization never covered: the full principal 72h after the lock when no " +
      "refund instructions reached this chain (e.g. it missed the clearing deadline), or the recorded " +
      "refund portion post-finalize. Permissionless and always pays the stored bidder. Requires OUTBE_PRIVATE_KEY.",
    { worldwideDay: worldwideDayArg, bidder: accountArg, network: networkArg.optional(), wait: waitArg },
    handler(async ({ worldwideDay, bidder, network, wait }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const account = requireAccount();
      const who = bidder ? getAddress(bidder) : account.address;
      const data = encodeFunctionData({ abi: ESCROW_ABI, functionName: "claimRefund", args: [worldwideDay, who] });
      const receipt = await submit(n, addr(n, "escrow"), data, 0n, wait);
      return ok({ network: n.name, worldwideDay, bidder: who, ...receipt });
    }),
  );

  // --- Bid funding (BSC payment token -> EscrowAdapter) ----------------------
  server.tool(
    "intex_payment_allowance",
    "Payment-token allowance granted to the EscrowAdapter and the account's balance, with token decimals/symbol.",
    { account: accountArg, network: networkArg.optional() },
    handler(async ({ account, network }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const who = whoever(account);
      const token = addr(n, "paymentToken");
      const escrow = addr(n, "escrow");
      const [allowance, balance, decimals, symbol] = (await Promise.all([
        n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "allowance", args: [who, escrow] }),
        n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "balanceOf", args: [who] }),
        n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "decimals" }),
        n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "symbol" }),
      ])) as [bigint, bigint, number, string];
      const d = Number(decimals);
      return ok({
        network: n.name,
        account: who,
        token: { address: token, symbol, decimals: d },
        escrow,
        allowance: { raw: allowance.toString(), value: formatUnits(allowance, d) },
        balance: { raw: balance.toString(), value: formatUnits(balance, d) },
      });
    }),
  );

  server.tool(
    "intex_payment_approve",
    "Manually approve the EscrowAdapter to pull the payment token. Usually unnecessary — auction_bid_reveal " +
      "auto-approves what it needs. Pass amount in token units (e.g. \"100\") or max=true. Requires OUTBE_PRIVATE_KEY.",
    {
      amount: z.string().optional().describe('token amount to approve, e.g. "100"'),
      max: z.boolean().optional().describe("approve the maximum instead of a fixed amount"),
      network: networkArg.optional(),
      wait: waitArg,
    },
    handler(async ({ amount, max, network, wait }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      requireAccount();
      if (!max && amount === undefined) throw new Error('pass amount (e.g. "100") or max=true');
      const value = max ? maxUint256 : parseUnits(amount as string, (await paymentMeta(n)).decimals);
      const token = addr(n, "paymentToken");
      const escrow = addr(n, "escrow");
      const data = encodeFunctionData({ abi: ERC20_ABI, functionName: "approve", args: [escrow, value] });
      const receipt = await submit(n, token, data, 0n, wait);
      return ok({ network: n.name, token, escrow, approved: max ? "max" : (amount as string), ...receipt });
    }),
  );

  // --- Bridge BSC -> outbe (IntexNFT1155Bridge, signed) ----------------------

  async function buildSendParam(n: Network, series: number, amount: bigint, recipient: Address) {
    const ids = (await n.client.readContract({
      address: addr(n, "nft"),
      abi: NFT_ABI,
      functionName: "tokenIds",
      args: [series],
    })) as [bigint, bigint];
    return {
      dstChainId: bridgeDstChainId(n.name),
      to: pad(recipient, { size: 32 }),
      tokenId: ids[0], // issued token id
      amount,
    };
  }


  server.tool(
    "intex_bridge_quote",
    "Bridge native fee to move an Intex NFT from BSC to outbe. Voluntary bridging is allowed while the " +
      "series is Issued or Qualified (Called is auto-bridged by the system).",
    { series: seriesArg, amount: amountArg, recipient: recipientArg, network: networkArg.optional() },
    handler(async ({ series, amount, recipient, network }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const to = recipient ? getAddress(recipient) : whoever();
      const sp = await buildSendParam(n, series, BigInt(amount), to);
      const fee = (await n.client.readContract({
        address: addr(n, "nftBridge"),
        abi: NFT_BRIDGE_ABI,
        functionName: "quoteSend",
        args: [sp],
      })) as bigint;
      return ok({
        network: n.name,
        series,
        tokenId: sp.tokenId.toString(),
        dstChainId: sp.dstChainId,
        recipient: to,
        fee: { nativeFee: { raw: fee.toString(), value: formatUnits(fee, 18) } },
      });
    }),
  );

  server.tool(
    "intex_bridge_send",
    "Bridge a Qualified Intex NFT from BSC to outbe (voluntary, holder-initiated) to settle there. Only " +
      "works once the series is Qualified — Issued cannot bridge, and Called is auto-bridged by the system, " +
      "not via this tool. The bridge burns your token directly (role-gated), so no approval is needed. " +
      "Auto-quotes the native fee (paid as value). Requires OUTBE_PRIVATE_KEY.",
    { series: seriesArg, amount: amountArg, recipient: recipientArg, network: networkArg.optional(), wait: waitArg },
    handler(async ({ series, amount, recipient, network, wait }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const account = requireAccount();
      const bridge = addr(n, "nftBridge");
      const to = recipient ? getAddress(recipient) : account.address;
      const sp = await buildSendParam(n, series, BigInt(amount), to);
      const fee = (await n.client.readContract({
        address: bridge,
        abi: NFT_BRIDGE_ABI,
        functionName: "quoteSend",
        args: [sp],
      })) as bigint;
      const data = encodeFunctionData({ abi: NFT_BRIDGE_ABI, functionName: "send", args: [sp] });
      const receipt = await submit(n, bridge, data, fee, wait);
      return ok({
        network: n.name,
        series,
        tokenId: sp.tokenId.toString(),
        recipient: to,
        fee: { raw: fee.toString(), value: formatUnits(fee, 18) },
        ...receipt,
      });
    }),
  );

  // --- Settlement + Promis (outbe IntexFactory, signed) ----------------------
  server.tool(
    "auction_bid_settle",
    "Settlement step 1: pay the strike and turn Issued Intexes into Settled (Promis is minted later via " +
      "intex_promis_mine). Defaults to your own wallet; pass holder only if that holder authorized you via " +
      "auction_settler_set. Allowed when the series is Qualified (voluntary) or Called (forced, " +
      "within the call period). The Settled token (soulbound) and the later Promis go to the SIGNING wallet, " +
      "not to holder; since the MCP signs with one key, to land them on a different wallet that wallet must " +
      "settle/mine itself — Issued is transferable on BSC only while the series is Issued/Qualified (Called " +
      "freezes transfers), so move it before the call. Requires OUTBE_PRIVATE_KEY.",
    { series: seriesArg, amount: amountArg, holder: accountArg, network: networkArg.optional(), wait: waitArg },
    handler(async ({ series, amount, holder, network, wait }) => {
      const n = await resolveNetwork(network ?? "outbe-testnet");
      const account = requireAccount();
      const intexHolder = holder ? getAddress(holder) : account.address;
      const data = encodeFunctionData({ abi: FACTORY_ABI, functionName: "settle", args: [series, intexHolder, BigInt(amount)] });
      const receipt = await submit(n, addr(n, "factory"), data, 0n, wait);
      return ok({ network: n.name, series, intexHolder, amount, self: intexHolder === account.address, ...receipt });
    }),
  );

  server.tool(
    "auction_settler_set",
    "Authorize another wallet to settle your position in a series. Call this from the holder wallet before " +
      "that wallet can settle on your behalf. Requires OUTBE_PRIVATE_KEY.",
    { series: seriesArg, settler: z.string().describe("0x address to authorize"), network: networkArg.optional(), wait: waitArg },
    handler(async ({ series, settler, network, wait }) => {
      const n = await resolveNetwork(network ?? "outbe-testnet");
      requireAccount();
      const data = encodeFunctionData({ abi: FACTORY_ABI, functionName: "setAuthorizedSettler", args: [series, getAddress(settler)] });
      const receipt = await submit(n, addr(n, "factory"), data, 0n, wait);
      return ok({ network: n.name, series, settler: getAddress(settler), ...receipt });
    }),
  );

  server.tool(
    "intex_promis_mine",
    "Settlement step 2: burn your Settled Intexes and mine Promis to your own wallet (run auction_bid_settle " +
      "first). The proof-of-work nonce is computed locally; you give only series and amount. Requires OUTBE_PRIVATE_KEY.",
    { series: seriesArg, amount: amountArg, network: networkArg.optional(), wait: waitArg },
    handler(async ({ series, amount, network, wait }) => {
      const n = await resolveNetwork(network ?? "outbe-testnet");
      const account = requireAccount();
      const holder = account.address;
      const amt = BigInt(amount);
      const sd = (await n.client.readContract({
        address: addr(n, "intex"),
        abi: INTEX_ABI,
        functionName: "seriesData",
        args: [series],
      })) as { promisLoadMinor: bigint };
      const promisAmount = sd.promisLoadMinor * amt;
      // seq = this holder's prior mints for the series (feeds the PoW preimage).
      const logs = await n.client.getLogs({
        address: addr(n, "factory"),
        event: PROMIS_MINED_EVENT,
        args: { seriesId: series, holder },
        fromBlock: 0n,
        toBlock: "latest",
      });
      const seq = logs.length;
      const pow = grindNonce(holder, promisAmount, series, seq);
      const data = encodeFunctionData({ abi: FACTORY_ABI, functionName: "minePromis", args: [series, amt, pow.nonce] });
      const receipt = await submit(n, addr(n, "factory"), data, 0n, wait);
      return ok({
        network: n.name,
        series,
        amount: amt.toString(),
        promisAmount: promisAmount.toString(),
        pow: { nonce: pow.nonce.toString(), iterations: pow.iterations, hash: pow.hash, difficulty: POW_DIFFICULTY, seq },
        ...receipt,
      });
    }),
  );

  server.tool(
    "intex_promis_balance",
    "Promis balance for an address on outbe.",
    { account: accountArg, network: networkArg.optional() },
    handler(async ({ account, network }) => {
      const n = await resolveNetwork(network ?? "outbe-testnet");
      const who = whoever(account);
      const bal = (await n.client.readContract({
        address: addr(n, "promis"),
        abi: ERC20_ABI,
        functionName: "balanceOf",
        args: [who],
      })) as bigint;
      return ok({ network: n.name, account: who, balance: { raw: bal.toString(), value: formatUnits(bal, 18) } });
    }),
  );
}
