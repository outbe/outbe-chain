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
  ERC20_ABI,
  FACTORY_ABI,
  type IntexAddresses,
  NETWORKS,
  NFT_ABI,
  ONFT_ABI,
  INTEX_ABI,
  bridgeDstEid,
  intexAddress,
} from "../intex/registry.js";
import { auctionStage, epochIso, intexState, intexStatus, isActiveStage } from "../intex/format.js";
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

// Auction series ids are WorldwideDay dates (yyyymmdd), one per day. Active
// auctions are discovered by probing getAuctionStage across a date window — a few
// cheap point reads — rather than scanning logs, which public RPCs range-limit.
const DEFAULT_DAYS_BACK = 2;
const DEFAULT_DAYS_AHEAD = 10;
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

  // Bids are priced in the auction's payment token (e.g. USDT, 6 decimals); the
  // user gives a human decimal and the MCP scales it. Cached per network so
  // commit and reveal scale identically, and so outputs can name the token.
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
  const paymentDecimals = async (n: Network) => (await paymentMeta(n)).decimals;

  /** Scale a human decimal price to the raw uint64 bidPrice the contract expects. */
  async function toBidPrice(n: Network, price: string): Promise<bigint> {
    const raw = parseUnits(price, await paymentDecimals(n));
    if (raw > 0xffff_ffff_ffff_ffffn) throw new Error(`price ${price} exceeds uint64 at the token's decimals`);
    return raw;
  }

  // --- shared argument schemas ---
  const networkArg = z.string().describe(`network name (one of: ${NETWORKS.map((d) => d.name).join(", ")})`);
  const accountArg = z.string().optional().describe("0x address to query (default: the configured signer)");
  const seriesArg = z.number().int().describe("series id");
  const quantityArg = z.number().int().describe("bid quantity (uint16)");
  const priceArg = z
    .string()
    .describe('bid price per intex in payment-token units, e.g. "1.5" (scaled by the token decimals; min from intex_auction_info)');
  const amountArg = z.string().describe("amount as the raw on-chain integer");
  const recipientArg = z.string().optional().describe("recipient on outbe (default: the signer)");
  const waitArg = z.boolean().optional().describe("wait for the receipt (default true)");

  // --- Series ledger (outbe Intex) -----------------------------------
  server.tool(
    "intex_series_info",
    "Canonical series record from the outbe Intex: size, strike, price floors, " +
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
        costAmount: { raw: d.costAmountMinor.toString(), scale: "payment-token decimals" },
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
    "intex_my_holdings",
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
  const auctionStageOf = (n: Network, series: number) =>
    n.client.readContract({
      address: addr(n, "auction"),
      abi: AUCTION_ABI,
      functionName: "getAuctionStage",
      args: [series],
    }) as Promise<number>;

  /** Probe getAuctionStage across a yyyymmdd date window; drop dates with no auction. */
  async function discoverByDate(n: Network, fromDate: number, toDate: number): Promise<{ series: number; stage: number }[]> {
    const probed = await Promise.all(
      ymdRange(fromDate, toDate).map(async (series) => {
        try {
          return { series, stage: await auctionStageOf(n, series) };
        } catch {
          return null; // getAuctionStage reverts AuctionNotFound for empty dates
        }
      }),
    );
    return probed.filter((x): x is { series: number; stage: number } => x !== null);
  }

  server.tool(
    "intex_active_auctions",
    "Active Intex auctions and their stage. Series ids are dates (yyyymmdd); probes a date window " +
      "(default today-2..+10, override via from_date/to_date). Active = CommittingBids or RevealingBids; " +
      "pass include_all for every stage.",
    {
      network: networkArg.optional(),
      include_all: z.boolean().optional(),
      from_date: z.number().int().optional().describe("window start yyyymmdd (default today-2)"),
      to_date: z.number().int().optional().describe("window end yyyymmdd (default today+10)"),
    },
    handler(async ({ network, include_all, from_date, to_date }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const today = todayYmd();
      const from = from_date ?? ymdShift(today, -DEFAULT_DAYS_BACK);
      const to = to_date ?? ymdShift(today, DEFAULT_DAYS_AHEAD);
      const probed = await discoverByDate(n, from, to);
      const auctions = probed.map((p) => ({ series: p.series, stage: auctionStage(p.stage) }));
      const filtered = include_all ? auctions : auctions.filter((au) => isActiveStage(au.stage.code));
      return ok({ network: n.name, window: { from, to }, count: filtered.length, auctions: filtered });
    }),
  );

  server.tool(
    "intex_auction_info",
    "One auction's stage, schedule (commit/reveal/issuance ends in UTC), and params (sizes, min bid " +
      "price/quantity, strike, floor) in the payment token. Bids are sealed: the bid counts and clearing " +
      "result stay 0 until clearing runs after reveal, so 0 here does NOT mean there are no participants.",
    { series: seriesArg, network: networkArg.optional() },
    handler(async ({ series, network }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const [stage, info, meta] = await Promise.all([
        auctionStageOf(n, series),
        n.client.readContract({ address: addr(n, "auction"), abi: AUCTION_ABI, functionName: "getAuctionInfo", args: [series] }),
        paymentMeta(n),
      ]);
      const dec = meta.decimals;
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
        paymentToken: { symbol: meta.symbol, decimals: dec },
        params: {
          intexSize: d.params.intexSize.toString(),
          // bid price, strike and the derived coen floor are all in payment-token
          // units (coenPriceFloor = strike * 1.08 / intexSize); show human values.
          minIntexBidPrice: { raw: d.params.minIntexBidPrice.toString(), value: formatUnits(d.params.minIntexBidPrice, dec) },
          intexStrikePrice: { raw: d.params.intexStrikePrice.toString(), value: formatUnits(d.params.intexStrikePrice, dec) },
          coenPriceFloor: { raw: d.params.coenPriceFloor.toString(), value: formatUnits(d.params.coenPriceFloor, dec) },
          minIntexBidQuantity: Number(d.params.minIntexBidQuantity),
        },
        result: {
          note: "populated only after clearing",
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
    { account: accountArg, series: seriesArg.optional(), network: networkArg.optional() },
    handler(async ({ account, series, network }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const who = whoever(account);
      let targets: number[];
      if (series !== undefined) {
        targets = [series];
      } else {
        const today = todayYmd();
        const probed = await discoverByDate(n, ymdShift(today, -DEFAULT_DAYS_BACK), ymdShift(today, DEFAULT_DAYS_AHEAD));
        targets = probed.filter((x) => isActiveStage(x.stage)).map((x) => x.series).sort((x, y) => x - y);
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

  // --- Bid commit / reveal (BSC IntexAuction, signed) ------------------------
  async function signReveal(n: Network, account: Account, series: number, quantity: number, bidPrice: bigint): Promise<Hex> {
    const typedData = revealBidTypedData({
      chainId: n.chainId,
      verifyingContract: addr(n, "auction"),
      seriesId: series,
      bidder: account.address,
      quantity,
      bidPrice,
    });
    if (!account.signTypedData) throw new Error("the configured account cannot sign typed data");
    return account.signTypedData(typedData);
  }

  server.tool(
    "intex_commit_bid",
    "Commit a sealed Intex bid: signs the EIP-712 RevealBid and submits keccak256(signature) as the commit " +
      "hash (no separate salt). No token approval needed — the escrow is funded only at reveal. IMPORTANT: " +
      "save your (series, quantity, price); you must repeat them to reveal, they can't be recovered on-chain, " +
      "and are only remembered this session. Requires OUTBE_PRIVATE_KEY.",
    { series: seriesArg, quantity: quantityArg, price: priceArg, network: networkArg.optional(), wait: waitArg },
    handler(async ({ series, quantity, price, network, wait }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const account = requireAccount();
      const bidPrice = await toBidPrice(n, price);
      const signature = await signReveal(n, account, series, quantity, bidPrice);
      const hash = commitHash(signature);
      const data = encodeFunctionData({ abi: AUCTION_ABI, functionName: "commitBid", args: [series, hash] });
      const receipt = await submit(n, addr(n, "auction"), data, 0n, wait);
      return ok({
        network: n.name,
        series,
        quantity,
        price,
        priceRaw: bidPrice.toString(),
        commitHash: hash,
        ...receipt,
        reminder:
          `Record series=${series}, quantity=${quantity}, price=${price} — required to reveal, ` +
          `not recoverable on-chain, remembered only this session.`,
      });
    }),
  );

  server.tool(
    "intex_reveal_bid",
    "Reveal a committed Intex bid: re-derives the same signature from (series, quantity, price) and submits " +
      "revealBid; the escrow then pulls quantity*price of the payment token. Auto-approves the escrow first " +
      "if the allowance is short (no separate approve step). Requires OUTBE_PRIVATE_KEY.",
    { series: seriesArg, quantity: quantityArg, price: priceArg, network: networkArg.optional(), wait: waitArg },
    handler(async ({ series, quantity, price, network, wait }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const account = requireAccount();
      const { decimals: dec, symbol } = await paymentMeta(n);
      const bidPrice = await toBidPrice(n, price);

      // Reveal makes the escrow pull quantity*price of the payment token, so the
      // allowance must cover it. Handle that here so the user needs no separate
      // approve step — but report it so the spend is never silent.
      const lockAmount = BigInt(quantity) * bidPrice;
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
        note = `Reveal locks ${lockHuman} ${symbol} (${quantity} x ${price}) in escrow. Allowance was short, so the escrow was approved for ${lockHuman} ${symbol} first, then the bid was revealed.`;
      } else {
        note = `Reveal locks ${lockHuman} ${symbol} (${quantity} x ${price}) in escrow; allowance already covered it, no approval needed.`;
      }

      const signature = await signReveal(n, account, series, quantity, bidPrice);
      const data = encodeFunctionData({
        abi: AUCTION_ABI,
        functionName: "revealBid",
        args: [series, quantity, bidPrice, BigInt(n.chainId), signature],
      });
      const receipt = await submit(n, addr(n, "auction"), data, 0n, wait);
      return ok({ network: n.name, series, quantity, price, priceRaw: bidPrice.toString(), locked: lockHuman, autoApprove, note, ...receipt });
    }),
  );

  server.tool(
    "intex_cancel_commit",
    "Cancel a committed bid for a series before the reveal stage. Requires OUTBE_PRIVATE_KEY.",
    { series: seriesArg, network: networkArg.optional(), wait: waitArg },
    handler(async ({ series, network, wait }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      requireAccount();
      const data = encodeFunctionData({ abi: AUCTION_ABI, functionName: "cancelCommit", args: [series] });
      const receipt = await submit(n, addr(n, "auction"), data, 0n, wait);
      return ok({ network: n.name, series, ...receipt });
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
    "intex_approve_payment",
    "Manually approve the EscrowAdapter to pull the payment token. Usually unnecessary — intex_reveal_bid " +
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
      const value = max ? maxUint256 : parseUnits(amount as string, await paymentDecimals(n));
      const token = addr(n, "paymentToken");
      const escrow = addr(n, "escrow");
      const data = encodeFunctionData({ abi: ERC20_ABI, functionName: "approve", args: [escrow, value] });
      const receipt = await submit(n, token, data, 0n, wait);
      return ok({ network: n.name, token, escrow, approved: max ? "max" : (amount as string), ...receipt });
    }),
  );

  // --- Bridge BSC -> outbe (ONFT1155Adapter, signed) -------------------------

  async function buildSendParam(n: Network, series: number, amount: bigint, recipient: Address) {
    const ids = (await n.client.readContract({
      address: addr(n, "nft"),
      abi: NFT_ABI,
      functionName: "tokenIds",
      args: [series],
    })) as [bigint, bigint];
    return {
      dstEid: bridgeDstEid(n.name),
      to: pad(recipient, { size: 32 }),
      tokenId: ids[0], // issued token id
      amount,
      extraOptions: "0x" as Hex,
      composeMsg: "0x" as Hex,
    };
  }


  server.tool(
    "intex_bridge_quote",
    "LayerZero native fee to bridge a Qualified Intex NFT from BSC to outbe. Bridging is only allowed once " +
      "a series is Qualified (Issued cannot bridge; Called is auto-bridged by the system).",
    { series: seriesArg, amount: amountArg, recipient: recipientArg, network: networkArg.optional() },
    handler(async ({ series, amount, recipient, network }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const to = recipient ? getAddress(recipient) : whoever();
      const sp = await buildSendParam(n, series, BigInt(amount), to);
      const fee = (await n.client.readContract({
        address: addr(n, "bridgeAdapter"),
        abi: ONFT_ABI,
        functionName: "quoteSend",
        args: [sp, false],
      })) as { nativeFee: bigint; lzTokenFee: bigint };
      return ok({
        network: n.name,
        series,
        tokenId: sp.tokenId.toString(),
        dstEid: sp.dstEid,
        recipient: to,
        fee: { nativeFee: { raw: fee.nativeFee.toString(), value: formatUnits(fee.nativeFee, 18) } },
      });
    }),
  );

  server.tool(
    "intex_bridge_approve",
    "One-time approval for the bridge adapter to move your Intex NFTs (setApprovalForAll), needed before " +
      "intex_bridge_nft. Requires OUTBE_PRIVATE_KEY.",
    { network: networkArg.optional(), wait: waitArg },
    handler(async ({ network, wait }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      requireAccount();
      const adapter = addr(n, "bridgeAdapter");
      const data = encodeFunctionData({ abi: NFT_ABI, functionName: "setApprovalForAll", args: [adapter, true] });
      const receipt = await submit(n, addr(n, "nft"), data, 0n, wait);
      return ok({ network: n.name, nft: addr(n, "nft"), adapter, ...receipt });
    }),
  );

  server.tool(
    "intex_bridge_nft",
    "Bridge a Qualified Intex NFT from BSC to outbe (voluntary, holder-initiated) to settle there. Only " +
      "works once the series is Qualified — Issued cannot bridge, and Called is auto-bridged by the system, " +
      "not via this tool. Auto-quotes the LayerZero fee (paid as native value) and needs intex_bridge_approve " +
      "first. Requires OUTBE_PRIVATE_KEY.",
    { series: seriesArg, amount: amountArg, recipient: recipientArg, network: networkArg.optional(), wait: waitArg },
    handler(async ({ series, amount, recipient, network, wait }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const account = requireAccount();
      const adapter = addr(n, "bridgeAdapter");
      const approved = (await n.client.readContract({
        address: addr(n, "nft"),
        abi: NFT_ABI,
        functionName: "isApprovedForAll",
        args: [account.address, adapter],
      })) as boolean;
      if (!approved) throw new Error("NFT not approved for the bridge adapter — run intex_bridge_approve first");
      const to = recipient ? getAddress(recipient) : account.address;
      const sp = await buildSendParam(n, series, BigInt(amount), to);
      const fee = (await n.client.readContract({
        address: adapter,
        abi: ONFT_ABI,
        functionName: "quoteSend",
        args: [sp, false],
      })) as { nativeFee: bigint; lzTokenFee: bigint };
      const data = encodeFunctionData({ abi: ONFT_ABI, functionName: "send", args: [sp, fee, account.address] });
      const receipt = await submit(n, adapter, data, fee.nativeFee, wait);
      return ok({
        network: n.name,
        series,
        tokenId: sp.tokenId.toString(),
        recipient: to,
        lzFee: { raw: fee.nativeFee.toString(), value: formatUnits(fee.nativeFee, 18) },
        ...receipt,
      });
    }),
  );

  // --- Settlement + Promis (outbe IntexFactory, signed) ----------------------
  server.tool(
    "intex_settle",
    "Settlement step 1: pay the strike and turn Issued Intexes into Settled (Promis is minted later via " +
      "intex_mine_promis). Defaults to your own wallet; pass holder only if that holder authorized you via " +
      "intex_set_authorized_settler. Allowed when the series is Qualified (voluntary) or Called (forced, " +
      "within the call period). The Settled token (soulbound) and the later Promis go to the SIGNING wallet, " +
      "not to holder; since the MCP signs with one key, to land them on a different wallet that wallet must " +
      "settle/mine itself — Issued is freely transferable on BSC, so move it there first. Requires OUTBE_PRIVATE_KEY.",
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
    "intex_set_authorized_settler",
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
    "intex_mine_promis",
    "Settlement step 2: burn your Settled Intexes and mine Promis to your own wallet (run intex_settle " +
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
