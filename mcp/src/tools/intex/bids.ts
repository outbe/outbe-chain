import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { type Account, type Hex, encodeFunctionData, formatUnits, getAddress } from "viem";
import type { Network } from "../../net/resolver.js";
import { requireAccount } from "../../net/tx.js";
import { networkName, waitFlag } from "../schemas.js";
import { ensureAllowance } from "../../net/erc20.js";
import { handler, ok } from "../util.js";
import { AUCTION_ABI, ESCROW_ABI } from "../../intex/registry.js";
import { commitHash, revealBidTypedData } from "../../intex/bid.js";
import { toBidRate, wcoenLockAmount } from "../../intex/units.js";
import { addr } from "../../intex/reads.js";
import { accountArg, bidUnitsArg, issuanceCurrencyArg, rateArg, referenceCurrencyArg, worldwideDayArg } from "./args.js";
import type { IntexDeps } from "./deps.js";

interface Bid {
  worldwideDay: number;
  units: number;
  bidRate: bigint;
  issuanceCurrency: number;
  referenceCurrency: number;
}

async function signReveal(n: Network, account: Account, bid: Bid): Promise<Hex> {
  const typedData = revealBidTypedData({
    chainId: n.chainId,
    verifyingContract: addr(n, "auction"),
    worldwideDay: bid.worldwideDay,
    bidder: account.address,
    units: bid.units,
    bidRate: Number(bid.bidRate),
    issuanceCurrency: bid.issuanceCurrency,
    referenceCurrency: bid.referenceCurrency,
  });
  if (!account.signTypedData) throw new Error("the configured account cannot sign typed data");
  return account.signTypedData(typedData);
}

/** Sealed bid commit, reveal and the escrow claims. */
export function registerBidTools(server: McpServer, deps: IntexDeps): void {
  const { ctx, submit, paymentMeta, target } = deps;
  server.tool(
    "auction_bid_commit",
    "Commit a sealed Intex bid: signs the EIP-712 RevealBid and submits keccak256(signature) as the commit " +
      "hash (no separate salt). When the auction carries an entry bond (commitBondMinor > 0), commitBid pulls " +
      "it into escrow in the same transaction - the tool auto-approves the escrow if the allowance is short. " +
      "The bond returns at reveal/cancel; a green-day no-reveal locks it for 24 hours past revealEnd " +
      "(intex_claim_commit_bond). IMPORTANT: save your (worldwideDay, units, rate, currencies); you must repeat " +
      "them to reveal, they can't be recovered on-chain, and are only remembered this session. Requires OUTBE_PRIVATE_KEY.",
    {
      worldwideDay: worldwideDayArg,
      units: bidUnitsArg,
      rate: rateArg,
      issuanceCurrency: issuanceCurrencyArg,
      referenceCurrency: referenceCurrencyArg,
      network: networkName.optional(),
      wait: waitFlag,
    },
    handler(async ({ worldwideDay, units, rate, issuanceCurrency, referenceCurrency, network, wait }) => {
      const n = await target(network);
      const account = requireAccount(ctx);
      const bidRate = toBidRate(rate);

      // Entry bond: the escrow pulls it inside commitBid, so cover the allowance first.
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
        const { token, decimals: dec, symbol } = await paymentMeta(n);
        const bondHuman = formatUnits(bond, dec);
        const approval = await ensureAllowance(ctx, n, {
          token,
          spender: addr(n, "escrow"),
          amount: bond,
        });
        if (approval) autoApprove = { txHash: approval, amount: bond.toString() };
        note =
          `Commit locks a ${bondHuman} ${symbol} entry bond in escrow; it returns at reveal/cancel. ` +
          `A green-day no-reveal keeps it locked until 24 hours past revealEnd (intex_claim_commit_bond).`;
      }

      const signature = await signReveal(n, account, { worldwideDay, units, bidRate, issuanceCurrency, referenceCurrency });
      const hash = commitHash(signature);
      const data = encodeFunctionData({ abi: AUCTION_ABI, functionName: "commitBid", args: [worldwideDay, hash] });
      const receipt = await submit(n, addr(n, "auction"), data, 0n, wait);
      return ok({
        network: n.name,
        worldwideDay,
        units,
        rate,
        bidRate: bidRate.toString(),
        issuanceCurrency,
        referenceCurrency,
        commitHash: hash,
        bond: bond.toString(),
        autoApprove,
        note,
        ...receipt,
        reminder:
          `Record worldwideDay=${worldwideDay}, units=${units}, rate=${rate} - required to reveal, ` +
          `not recoverable on-chain, remembered only this session.`,
      });
    }),
  );

  server.tool(
    "auction_bid_reveal",
    "Reveal a committed Intex bid: re-derives the same signature from (worldwideDay, units, rate, currencies) " +
    "and submits revealBid; the escrow calculates units * protocol-6 PROMIS load * rate / 1e6, then converts " +
      "that result by 1e12 into native-18 WCOEN for the lock. The reference currency must be one the day prices, the issuance currency any " +
      "1..999 code. Auto-approves the escrow first if the allowance is short. Requires OUTBE_PRIVATE_KEY.",
    {
      worldwideDay: worldwideDayArg,
      units: bidUnitsArg,
      rate: rateArg,
      issuanceCurrency: issuanceCurrencyArg,
      referenceCurrency: referenceCurrencyArg,
      network: networkName.optional(),
      wait: waitFlag,
    },
    handler(async ({ worldwideDay, units, rate, issuanceCurrency, referenceCurrency, network, wait }) => {
      const n = await target(network);
      const account = requireAccount(ctx);
      const { decimals: dec, symbol } = await paymentMeta(n);
      const bidRate = toBidRate(rate);

      // Calculate in protocol-6 from the per-Intex PROMIS load and 1e6 rate,
      // then convert exactly once to native-18 WCOEN for the escrow boundary.
      const info = (await n.client.readContract({
        address: addr(n, "auction"),
        abi: AUCTION_ABI,
        functionName: "getAuctionInfo",
        args: [worldwideDay],
      })) as { params: { promisLoadMinor: bigint; commitBondMinor: bigint } };
      const strike = info.params.promisLoadMinor;
      const lockAmount = wcoenLockAmount(BigInt(units), strike, bidRate);
      const lockHuman = formatUnits(lockAmount, dec);
      const approval = await ensureAllowance(ctx, n, {
        token: (await paymentMeta(n)).token,
        spender: addr(n, "escrow"),
        amount: lockAmount,
      });
      const autoApprove = approval ? { txHash: approval, amount: lockAmount.toString() } : null;
      let note: string;
      if (approval) {
        note = `Reveal locks ${lockHuman} ${symbol} (${units} x strike x ${rate}) in escrow. Allowance was short, so the escrow was approved for ${lockHuman} ${symbol} first, then the bid was revealed.`;
      } else {
        note = `Reveal locks ${lockHuman} ${symbol} (${units} x strike x ${rate}) in escrow; allowance already covered it, no approval needed.`;
      }
      if (info.params.commitBondMinor > 0n) {
        note += ` The ${formatUnits(info.params.commitBondMinor, dec)} ${symbol} entry bond returns within the same transaction (released before the bid lock, so it can fund the bid).`;
      }

      const signature = await signReveal(n, account, { worldwideDay, units, bidRate, issuanceCurrency, referenceCurrency });
      const data = encodeFunctionData({
        abi: AUCTION_ABI,
        functionName: "revealBid",
        args: [
          worldwideDay,
          units,
          bidRate,
          issuanceCurrency,
          referenceCurrency,
          BigInt(n.chainId),
          signature,
        ],
      });
      const receipt = await submit(n, addr(n, "auction"), data, 0n, wait);
      return ok({ network: n.name, worldwideDay, units, rate, bidRate: bidRate.toString(), locked: lockHuman, autoApprove, note, ...receipt });
    }),
  );

  server.tool(
    "auction_bid_cancel",
    "Cancel a committed bid for a worldwide day before the reveal stage. Requires OUTBE_PRIVATE_KEY.",
    { worldwideDay: worldwideDayArg, network: networkName.optional(), wait: waitFlag },
    handler(async ({ worldwideDay, network, wait }) => {
      const n = await target(network);
      requireAccount(ctx);
      const data = encodeFunctionData({ abi: AUCTION_ABI, functionName: "cancelCommit", args: [worldwideDay] });
      const receipt = await submit(n, addr(n, "auction"), data, 0n, wait);
      return ok({ network: n.name, worldwideDay, ...receipt });
    }),
  );

  server.tool(
    "intex_claim_commit_bond",
    "Reclaim an entry bond left behind by a no-reveal commit. Permissionless and always pays the stored " +
      "bidder: a cancelled (red-day) auction releases immediately, otherwise the bond is claimable only " +
      "24 hours past revealEnd. Requires OUTBE_PRIVATE_KEY.",
    { worldwideDay: worldwideDayArg, bidder: accountArg, network: networkName.optional(), wait: waitFlag },
    handler(async ({ worldwideDay, bidder, network, wait }) => {
      const n = await target(network);
      const account = requireAccount(ctx);
      const who = bidder ? getAddress(bidder) : account.address;
      const data = encodeFunctionData({ abi: AUCTION_ABI, functionName: "claimCommitBond", args: [worldwideDay, who] });
      const receipt = await submit(n, addr(n, "auction"), data, 0n, wait);
      return ok({ network: n.name, worldwideDay, bidder: who, ...receipt });
    }),
  );

  server.tool(
    "auction_claim_refund",
    "Collect what a bid lock still holds and close it: a winner's change over the clearing price, a loser's " +
      "whole principal once its day closed, or the whole principal 72h after the lock when no refund " +
      "instructions reached this chain (e.g. it missed the clearing deadline). Read the amount and the date " +
      "first with auction_bids_by_owner. Permissionless and always pays the stored bidder. Requires " +
      "OUTBE_PRIVATE_KEY.",
    { worldwideDay: worldwideDayArg, bidder: accountArg, network: networkName.optional(), wait: waitFlag },
    handler(async ({ worldwideDay, bidder, network, wait }) => {
      const n = await target(network);
      const account = requireAccount(ctx);
      const who = bidder ? getAddress(bidder) : account.address;
      const data = encodeFunctionData({ abi: ESCROW_ABI, functionName: "claimRefund", args: [worldwideDay, who] });
      const receipt = await submit(n, addr(n, "escrow"), data, 0n, wait);
      return ok({ network: n.name, worldwideDay, bidder: who, ...receipt });
    }),
  );
}
