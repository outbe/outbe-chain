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
} from "viem";
import { z } from "zod";
import { type Ctx, createCtx } from "../chain.js";
import { handler, ok } from "./util.js";
import {
  AUCTION_ABI,
  ERC20_ABI,
  type IntexAddresses,
  NETWORKS,
  NFT_ABI,
  REGISTRY_ABI,
  intexAddress,
} from "../intex/registry.js";
import { auctionStage, epochIso, intexState, intexStatus, isActiveStage } from "../intex/format.js";
import { commitHash, revealBidTypedData } from "../intex/bid.js";

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

  // --- Bid commit / reveal (BSC IntexAuction, signed) ------------------------
  const seriesArg = z.number().int().describe("series id");
  const quantityArg = z.number().int().describe("bid quantity (uint16)");
  const priceArg = z.string().describe("bid price as the raw on-chain uint64 (see intex_auction_info min price)");
  const waitArg = z.boolean().optional().describe("wait for the receipt (default true)");

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
    "Commit a sealed Intex bid. Signs the EIP-712 RevealBid (series, quantity, price) and submits its " +
      "keccak256 as the commit hash — there is no separate salt. IMPORTANT: record your (series, quantity, " +
      "price); you must supply them again to reveal, they cannot be recovered on-chain, and this assistant " +
      "only remembers them within the current session. Approve the payment token before revealing. " +
      "Requires OUTBE_PRIVATE_KEY.",
    { series: seriesArg, quantity: quantityArg, price: priceArg, network: networkArg.optional(), wait: waitArg },
    handler(async ({ series, quantity, price, network, wait }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const account = requireAccount();
      const bidPrice = BigInt(price);
      const signature = await signReveal(n, account, series, quantity, bidPrice);
      const hash = commitHash(signature);
      const data = encodeFunctionData({ abi: AUCTION_ABI, functionName: "commitBid", args: [series, hash] });
      const receipt = await submit(n, addr(n, "auction"), data, 0n, wait);
      return ok({
        network: n.name,
        series,
        quantity,
        price: bidPrice.toString(),
        commitHash: hash,
        ...receipt,
        reminder:
          `Record series=${series}, quantity=${quantity}, price=${bidPrice.toString()} — required to reveal, ` +
          `not recoverable on-chain, remembered only this session. Run intex_approve_payment before reveal.`,
      });
    }),
  );

  server.tool(
    "intex_reveal_bid",
    "Reveal a previously committed Intex bid. Re-derives the identical EIP-712 signature from (series, " +
      "quantity, price) and submits revealBid. The escrow pulls quantity*price of the payment token here, " +
      "so an allowance must already cover it (see intex_approve_payment). Requires OUTBE_PRIVATE_KEY.",
    { series: seriesArg, quantity: quantityArg, price: priceArg, network: networkArg.optional(), wait: waitArg },
    handler(async ({ series, quantity, price, network, wait }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const account = requireAccount();
      const bidPrice = BigInt(price);
      const signature = await signReveal(n, account, series, quantity, bidPrice);
      const data = encodeFunctionData({
        abi: AUCTION_ABI,
        functionName: "revealBid",
        args: [series, quantity, bidPrice, BigInt(n.chainId), signature],
      });
      const receipt = await submit(n, addr(n, "auction"), data, 0n, wait);
      return ok({ network: n.name, series, quantity, price: bidPrice.toString(), ...receipt });
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
    "Payment-token allowance granted to the EscrowAdapter and the owner's balance, with token decimals/symbol.",
    { owner: ownerArg, network: networkArg.optional() },
    handler(async ({ owner, network }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      const who = whoever(owner);
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
        owner: who,
        token: { address: token, symbol, decimals: d },
        escrow,
        allowance: { raw: allowance.toString(), value: formatUnits(allowance, d) },
        balance: { raw: balance.toString(), value: formatUnits(balance, d) },
      });
    }),
  );

  server.tool(
    "intex_approve_payment",
    "Approve the EscrowAdapter to pull the payment token (required before reveal). Pass amount as the raw " +
      "on-chain integer (must cover quantity*price), or max=true to approve the maximum. Requires OUTBE_PRIVATE_KEY.",
    {
      amount: z.string().optional().describe("raw token amount to approve"),
      max: z.boolean().optional().describe("approve the maximum uint256 instead of a fixed amount"),
      network: networkArg.optional(),
      wait: waitArg,
    },
    handler(async ({ amount, max, network, wait }) => {
      const n = await resolveNetwork(network ?? "bsc-testnet");
      requireAccount();
      if (!max && amount === undefined) throw new Error("pass amount (raw) or max=true");
      const value = max ? maxUint256 : BigInt(amount as string);
      const token = addr(n, "paymentToken");
      const escrow = addr(n, "escrow");
      const data = encodeFunctionData({ abi: ERC20_ABI, functionName: "approve", args: [escrow, value] });
      const receipt = await submit(n, token, data, 0n, wait);
      return ok({ network: n.name, token, escrow, approved: max ? "max" : value.toString(), ...receipt });
    }),
  );
}
