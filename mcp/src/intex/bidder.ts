import type { Address } from "viem";
import type { Network } from "../net/resolver.js";
import { DEFAULT_DAYS_AHEAD, DEFAULT_DAYS_BACK, todayYmd, ymdShift } from "./dates.js";
import { auctionStage, epochIso, fromSeriesId, isActiveStage, lockStatus } from "./format.js";
import { addr, auctionStageOf, discoverByDate } from "./reads.js";
import { AUCTION_ABI, ESCROW_ABI, NFT_ABI } from "./registry.js";

/** Lock status of a win whose payment left with the day's proceeds. */
const WON = 2;

export interface BidStatus {
  worldwideDay: number;
  committed: boolean;
  revealed: boolean;
}

/** The auction days to report: the one asked for, else every day found in the default window. */
export async function bidDays(n: Network, worldwideDay?: number): Promise<number[]> {
  if (worldwideDay !== undefined) return [worldwideDay];
  const today = todayYmd();
  // Closed days carry the locks that are still to be claimed, so the window is not filtered by stage.
  const probed = await discoverByDate(n, ymdShift(today, -DEFAULT_DAYS_BACK), ymdShift(today, DEFAULT_DAYS_AHEAD));
  return probed.map((x) => x.worldwideDay).sort((x, y) => x - y);
}

/** One day's commit/reveal state and the bidder's escrow money on `n`, with what to do next. */
export async function bidStatus(n: Network, who: Address, wwd: number): Promise<BidStatus> {
  const [commitHash, revealed, lock, bond] = await Promise.all([
    n.client.readContract({ address: addr(n, "auction"), abi: AUCTION_ABI, functionName: "committedBidsByHash", args: [wwd, who] }),
    n.client.readContract({ address: addr(n, "auction"), abi: AUCTION_ABI, functionName: "revealedBidsByBidder", args: [wwd, who] }),
    n.client.readContract({ address: addr(n, "escrow"), abi: ESCROW_ABI, functionName: "getBidLock", args: [wwd, who] }),
    n.client.readContract({ address: addr(n, "escrow"), abi: ESCROW_ABI, functionName: "getCommitBond", args: [wwd, who] }),
  ]);
  const committed = commitHash !== "0x" && /[1-9a-f]/i.test(commitHash.slice(2));
  const stage = await auctionStageOf(n, wwd);
  const out: BidStatus & Record<string, unknown> = { worldwideDay: wwd, committed, revealed, stage: auctionStage(stage) };
  const hints: string[] = [];
  if (bond.amount > 0n) {
    out.commitBond = { amount: bond.amount.toString(), lockedAt: epochIso(bond.lockedAt) };
    // A held bond during commit/reveal is normal (it returns at reveal/cancel). Past
    // that window, the bond remains because of a no-reveal commit.
    if (!revealed && !isActiveStage(stage)) {
      hints.push(
        "entry bond left by a no-reveal commit; reclaim via intex_claim_commit_bond (immediately on a cancelled day, else 24 hours past revealEnd)",
      );
    }
  }
  if (lock.status !== 0) {
    const [[, finalized], [claimable, claimableAt]] = await Promise.all([
      n.client.readContract({ address: addr(n, "escrow"), abi: ESCROW_ABI, functionName: "getAuctionStatus", args: [wwd] }),
      n.client.readContract({ address: addr(n, "escrow"), abi: ESCROW_ABI, functionName: "getClaimableRefund", args: [wwd, who] }),
    ]);
    const escrow: Record<string, unknown> = {
      lockedAmount: lock.lockedAmount.toString(),
      status: lockStatus(lock.status),
      finalized,
      claimable: claimable.toString(),
      // Zero means the escrow owes it now. A date means the day never finalized and the
      // full principal waits out the anomaly window.
      claimableAt: epochIso(claimableAt),
    };
    if (claimable > 0n) {
      hints.push(
        claimableAt === 0
          ? "claim it now via auction_claim_refund"
          : "no refund instructions reached this chain; claim the full lock via auction_claim_refund from claimableAt",
      );
    }
    if (lock.status === WON) {
      escrow.unitsWon = lock.quantity;
      hints.push(await issueHint(n, who, wwd, lock.quantity));
    }
    out.escrow = escrow;
  }
  if (hints.length > 0) out.hints = hints;
  return out;
}

/** What a win still has to do: the series to issue into and the deadline it closes at. */
async function issueHint(n: Network, who: Address, wwd: number, units: number): Promise<string> {
  const bid = await n.client.readContract({
    address: addr(n, "auction"),
    abi: AUCTION_ABI,
    functionName: "revealedBidOf",
    args: [wwd, who],
  });
  const seriesId = await n.client.readContract({
    address: addr(n, "nft"),
    abi: NFT_ABI,
    functionName: "seriesIdOf",
    args: [wwd, bid.issuanceCurrency, bid.referenceCurrency],
  });
  if (/^0x0*$/.test(seriesId)) {
    return `${units} won units wait for their series to reach this chain; issue them via auction_issue_intex once it lands`;
  }
  const d = await n.client.readContract({ address: addr(n, "nft"), abi: NFT_ABI, functionName: "readData", args: [seriesId] });
  const deadline = Number(d.calledAt) > 0 ? Number(d.calledAt) + Number(d.callTrigger.callNoticePeriod) : 0;
  const series = fromSeriesId(seriesId);
  if (deadline > 0 && Math.floor(Date.now() / 1000) > deadline) {
    return `series ${series} expired: auction_issue_intex closes the win without units and returns any change`;
  }
  const until = deadline > 0 ? ` before ${epochIso(deadline)?.iso}` : "";
  return `issue ${units} units of ${series} via auction_issue_intex${until}`;
}
