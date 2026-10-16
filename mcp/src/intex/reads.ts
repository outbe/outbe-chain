import {
  type AbiEvent,
  type Address,
  BaseError,
  ContractFunctionRevertedError,
  ContractFunctionZeroDataError,
  type Hex,
  getAbiItem,
  getAddress,
  pad,
} from "viem";
import { type DecodedDataUri, parseDataUri } from "../format.js";
import type { Network } from "../net/resolver.js";
import {
  AUCTION_ABI,
  ESCROW_ABI,
  FACTORY_ABI,
  INTEX_ABI,
  type IntexAddresses,
  NFT_ABI,
  ORIGIN_ROUTER_ABI,
  VAULT_ROUTER_ABI,
  NotConfiguredError,
  intexAddress,
  intexNftFromBlock,
} from "./registry.js";
import { ymdRange } from "./dates.js";

export const INTEX_EXERCISED_EVENT = getAbiItem({ abi: FACTORY_ABI, name: "IntexExercised" }) as AbiEvent;
const TRANSFER_SINGLE_EVENT = getAbiItem({ abi: NFT_ABI, name: "TransferSingle" }) as AbiEvent;
const TRANSFER_BATCH_EVENT = getAbiItem({ abi: NFT_ABI, name: "TransferBatch" }) as AbiEvent;

/** A contract call the chain answered with a revert, as opposed to a failure to reach it. */
export function isRevert(error: unknown): boolean {
  return error instanceof BaseError && error.walk((e) => e instanceof ContractFunctionRevertedError) !== null;
}

/** A contract call that returned no data, as a call to an address without code does. */
function isEmptyReturn(error: unknown): boolean {
  return error instanceof BaseError && error.walk((e) => e instanceof ContractFunctionZeroDataError) !== null;
}

export function addr(n: Network, key: keyof IntexAddresses): Address {
  return intexAddress(n, key);
}

/** Whether the series has qualified. Intex derives it from finalized daily VWAPs. */
const LOG_PAGE = 50_000n;

/** How many times `owner` has mined Promis from `series`: the PoW sequence of the next mine. Pages
 *  the log scan when the node limits the block range. */
export async function exerciseCount(n: Network, series: Hex, owner: Address): Promise<number> {
  const query = { address: addr(n, "factory"), event: INTEX_EXERCISED_EVENT, args: { seriesId: series, owner } };
  try {
    return (await n.client.getLogs({ ...query, fromBlock: 0n, toBlock: "latest" })).length;
  } catch {
    const latest = await n.client.getBlockNumber();
    let count = 0;
    for (let from = 0n; from <= latest; from += LOG_PAGE) {
      const to = from + LOG_PAGE - 1n < latest ? from + LOG_PAGE - 1n : latest;
      count += (await n.client.getLogs({ ...query, fromBlock: from, toBlock: to })).length;
    }
    return count;
  }
}

export async function seriesQualified(n: Network, series: Hex): Promise<boolean> {
  return await n.client.readContract({
    address: addr(n, "intex"),
    abi: INTEX_ABI,
    functionName: "isQualified",
    args: [series],
  });
}

/** Token ids the address holds now: candidates from inbound transfer logs, then a live balance read,
 *  since ERC-1155 has no owner enumeration. */
export async function ownedWithBalances(n: Network, owner: Address): Promise<[bigint[], bigint[]]> {
  const address = addr(n, "nft");
  const fromBlock = intexNftFromBlock(n.name);
  const [single, batch] = await Promise.all([
    n.client.getLogs({ address, event: TRANSFER_SINGLE_EVENT, args: { to: owner }, fromBlock, toBlock: "latest" }),
    n.client.getLogs({ address, event: TRANSFER_BATCH_EVENT, args: { to: owner }, fromBlock, toBlock: "latest" }),
  ]);

  const seen = new Set<bigint>();
  for (const log of single) {
    const id = (log.args as { id?: bigint }).id;
    if (id !== undefined) seen.add(id);
  }
  for (const log of batch) {
    for (const id of (log.args as { ids?: readonly bigint[] }).ids ?? []) seen.add(id);
  }

  const candidates = [...seen];
  if (candidates.length === 0) return [[], []];

  const balances = await n.client.readContract({
    address,
    abi: NFT_ABI,
    functionName: "balanceOfBatch",
    args: [candidates.map(() => owner), candidates],
  });

  const heldIds: bigint[] = [];
  const heldBalances: bigint[] = [];
  candidates.forEach((id, i) => {
    if (balances[i] > 0n) {
      heldIds.push(id);
      heldBalances.push(balances[i]);
    }
  });
  return [heldIds, heldBalances];
}

/** Per-token NFT metadata documents for a series; undefined when the chain has no NFT deployed. */
export async function seriesMetadata(
  n: Network,
  series: Hex,
): Promise<{ collection: DecodedDataUri; issued: DecodedDataUri; settled: DecodedDataUri } | undefined> {
  try {
    const nft = addr(n, "nft");
    const [issuedId, settledId] = await n.client.readContract({
      address: nft,
      abi: NFT_ABI,
      functionName: "tokenIds",
      args: [series],
    });
    const [collection, issued, settled] = await Promise.all([
      n.client.readContract({ address: nft, abi: NFT_ABI, functionName: "contractURI" }),
      n.client.readContract({ address: nft, abi: NFT_ABI, functionName: "uri", args: [issuedId] }),
      n.client.readContract({ address: nft, abi: NFT_ABI, functionName: "uri", args: [settledId] }),
    ]);
    return {
      collection: parseDataUri(collection),
      issued: parseDataUri(issued),
      settled: parseDataUri(settled),
    };
  } catch (error) {
    if (isRevert(error) || error instanceof NotConfiguredError) return undefined;
    throw error;
  }
}
/** Payment tokens a series accepts: the vault router's assets for either of its currencies.
 *  An issuance-currency token settles only while the trailing VWAP prices both legs. */
export async function settlementTokens(n: Network, series: Hex): Promise<`0x${string}`[]> {
  const d = await n.client.readContract({
    address: addr(n, "intex"),
    abi: INTEX_ABI,
    functionName: "getIntexSeries",
    args: [series],
  });
  const currencies = [d.referenceCurrency];
  if (d.issuanceCurrency !== d.referenceCurrency) currencies.push(d.issuanceCurrency);
  const perCurrency = await Promise.all(
    currencies.map(
      (iso) =>
        n.client.readContract({
          address: addr(n, "vaultRouter"),
          abi: VAULT_ROUTER_ABI,
          functionName: "referenceCurrencyAssets",
          args: [iso],
        }),
    ),
  );
  const seen = new Set<`0x${string}`>();
  for (const asset of perCurrency.flat()) seen.add(getAddress(asset));
  return [...seen];
}

/** The cost of settling `units` with `token`, in its minor units, priced and rounded once for the whole batch.
 *  `snapshotId` is the VWAP snapshot an issuance-currency payment names; it goes stale at the next hour. */
export async function quoteSettlement(
  n: Network,
  series: Hex,
  token: `0x${string}`,
  units: bigint,
): Promise<{ settlementCurrency: number; paymentMinor: bigint; snapshotId: bigint }> {
  const [settlementCurrency, paymentMinor, snapshotId] = await n.client.readContract({
    address: addr(n, "factory"),
    abi: FACTORY_ABI,
    functionName: "quoteSettlement",
    args: [series, token, units],
  });
  return { settlementCurrency: Number(settlementCurrency), paymentMinor, snapshotId };
}

export const auctionStageOf = (n: Network, worldwideDay: number) =>
  n.client.readContract({
    address: addr(n, "auction"),
    abi: AUCTION_ABI,
    functionName: "getAuctionStage",
    args: [worldwideDay],
  });

/** Probe getAuctionStage across a yyyymmdd date window; drop dates with no auction. */
export async function discoverByDate(n: Network, fromDate: number, toDate: number): Promise<{ worldwideDay: number; stage: number }[]> {
  const probed = await Promise.all(
    ymdRange(fromDate, toDate).map(async (worldwideDay) => {
      try {
        return { worldwideDay, stage: await auctionStageOf(n, worldwideDay) };
      } catch (error) {
        if (isRevert(error)) return null; // getAuctionStage reverts AuctionNotFound for empty dates
        throw error;
      }
    }),
  );
  return probed.filter((x): x is { worldwideDay: number; stage: number } => x !== null);
}

export interface BridgeSend {
  series: Hex;
  units: bigint;
  recipient: Address;
  dstChainId: number;
}

export async function bridgeSendParam(n: Network, { series, units, recipient, dstChainId }: BridgeSend) {
  const ids = await n.client.readContract({
    address: addr(n, "nft"),
    abi: NFT_ABI,
    functionName: "tokenIds",
    args: [series],
  });
  return {
    dstChainId,
    to: pad(recipient, { size: 32 }),
    tokenId: ids[0], // issued token id
    units,
  };
}

/** Chain ids the origin router fans auctions out to, read from the connected Outbe node. */
export async function intexTargets(outbe: Network): Promise<number[]> {
  const router = addr(outbe, "originRouter");
  try {
    const targets = await outbe.client.readContract({
      address: router,
      abi: ORIGIN_ROUTER_ABI,
      functionName: "targets",
    });
    return targets.map(Number);
  } catch (error) {
    if (!isRevert(error) && !isEmptyReturn(error)) throw error;
    throw new Error(`no Intex origin router answers at ${router} on ${outbe.name}; set OUTBE_INTEX_ORIGIN_ROUTER`);
  }
}

/** The token a target's escrow takes bids in. */
export async function escrowPaymentToken(n: Network): Promise<Address> {
  return await n.client.readContract({
    address: addr(n, "escrow"),
    abi: ESCROW_ABI,
    functionName: "paymentToken",
  });
}
