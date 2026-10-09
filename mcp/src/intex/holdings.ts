import type { Hex } from "viem";
import type { Network } from "../net/resolver.js";
import { epochIso, intexState, intexStatus, fromSeriesId } from "./format.js";
import { addr, seriesQualified } from "./reads.js";
import { NFT_ABI } from "./registry.js";

/** One held token id with its status and, for an Issued one, its series lifecycle. */
export async function holding(n: Network, tokenId: bigint, balance: bigint) {
  const status = (await n.client.readContract({
    address: addr(n, "nft"),
    abi: NFT_ABI,
    functionName: "statusOf",
    args: [tokenId],
  })) as number;
  const base = { tokenId: tokenId.toString(), balance: balance.toString(), status: intexStatus(status) };
  // An Issued token id is the series id itself, so the lifecycle is one read away. A Settled id
  // carries no deadline, because that position is already settled.
  if (base.status.name !== "Issued") return base;
  const seriesHex = `0x${tokenId.toString(16).padStart(28, "0")}` as Hex;
  try {
    const d = (await n.client.readContract({
      address: addr(n, "nft"),
      abi: NFT_ABI,
      functionName: "readData",
      args: [seriesHex],
    })) as { state: number; calledAt: bigint | number; callTrigger: { callNoticePeriod: bigint | number } };
    const settlementDeadline =
      Number(d.calledAt) > 0 ? Number(d.calledAt) + Number(d.callTrigger.callNoticePeriod) : 0;
    // Only outbe has the factory that derives it.
    const qualified = n.isOutbe ? await seriesQualified(n, seriesHex) : undefined;
    return {
      ...base,
      series: fromSeriesId(seriesHex),
      state: intexState(d.state === 0 && qualified ? 1 : d.state),
      ...(qualified === undefined ? {} : { qualified }),
      settlementDeadline: epochIso(settlementDeadline),
      expired: settlementDeadline > 0 && Math.floor(Date.now() / 1000) > settlementDeadline,
    };
  } catch {
    // A series the chain does not know is still a holding worth listing.
    return base;
  }
}
