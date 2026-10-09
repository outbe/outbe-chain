import {
  type AbiEvent,
  type Address,
  BaseError,
  ContractFunctionRevertedError,
  type Hex,
  getAbiItem,
  getAddress,
  pad,
} from "viem";
import { type DecodedDataUri, parseDataUri } from "../format.js";
import type { Network } from "../net/resolver.js";
import {
  AUCTION_ABI,
  FACTORY_ABI,
  INTEX_ABI,
  type IntexAddresses,
  NFT_ABI,
  VAULT_ROUTER_ABI,
  bridgeDstChainId,
  NotConfiguredError,
  intexAddress,
  intexNftFromBlock,
} from "./registry.js";
import { ymdRange } from "./dates.js";

export const PROMIS_MINED_EVENT = getAbiItem({ abi: FACTORY_ABI, name: "PromisMined" }) as AbiEvent;
export const TRANSFER_SINGLE_EVENT = getAbiItem({ abi: NFT_ABI, name: "TransferSingle" }) as AbiEvent;
export const TRANSFER_BATCH_EVENT = getAbiItem({ abi: NFT_ABI, name: "TransferBatch" }) as AbiEvent;

/** A contract call the chain answered with a revert, as opposed to a failure to reach it. */
export function isRevert(error: unknown): boolean {
  return error instanceof BaseError && error.walk((e) => e instanceof ContractFunctionRevertedError) !== null;
}

export function addr(n: Network, key: keyof IntexAddresses): Address {
  return intexAddress(n, key);
}

/** Whether the series has qualified. The outbe factory derives it from finalized daily VWAPs. */
export async function seriesQualified(n: Network, series: Hex): Promise<boolean> {
  return (await n.client.readContract({
    address: addr(n, "factory"),
    abi: FACTORY_ABI,
    functionName: "isSeriesQualified",
    args: [series],
  })) as boolean;
}

/** Token ids the address holds now: candidates from inbound transfer logs, then a live balance read.
 *  ERC-1155 carries no on-chain owner enumeration, so wallets and explorers derive holdings the same
 *  way. The scan starts at the pair's deployment block - see `intexNftFromBlock`. */
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

  const balances = (await n.client.readContract({
    address,
    abi: NFT_ABI,
    functionName: "balanceOfBatch",
    args: [candidates.map(() => owner), candidates],
  })) as bigint[];

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

/** Per-token NFT metadata documents for a series.
 *  Undefined when the chain has no NFT deployed. */
export async function seriesMetadata(
  n: Network,
  series: Hex,
): Promise<{ collection: DecodedDataUri; issued: DecodedDataUri; settled: DecodedDataUri } | undefined> {
  try {
    const nft = addr(n, "nft");
    const [issuedId, settledId] = (await n.client.readContract({
      address: nft,
      abi: NFT_ABI,
      functionName: "tokenIds",
      args: [series],
    })) as [bigint, bigint];
    const [collection, issued, settled] = (await Promise.all([
      n.client.readContract({ address: nft, abi: NFT_ABI, functionName: "contractURI" }),
      n.client.readContract({ address: nft, abi: NFT_ABI, functionName: "uri", args: [issuedId] }),
      n.client.readContract({ address: nft, abi: NFT_ABI, functionName: "uri", args: [settledId] }),
    ])) as [string, string, string];
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
/**
 * Payment tokens a series accepts: the vault router's assets for either of its
 * currencies. An issuance-currency token only settles while the trailing VWAP
 * window prices both COEN legs. `quoteSettlement` answers that question.
 */
export async function settlementTokens(n: Network, series: Hex): Promise<`0x${string}`[]> {
  const d = (await n.client.readContract({
    address: addr(n, "intex"),
    abi: INTEX_ABI,
    functionName: "seriesData",
    args: [series],
  })) as { referenceCurrency: number; issuanceCurrency: number };
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
        }) as Promise<readonly `0x${string}`[]>,
    ),
  );
  const seen = new Set<`0x${string}`>();
  for (const asset of perCurrency.flat()) seen.add(getAddress(asset));
  return [...seen];
}

/**
 * What settling `units` Intex of `series` with `token` costs, in that token's
 * minor units, and the ISO 4217 code the payment is denominated in. The chain
 * prices the whole operation, rounds once and applies its one-minor-unit
 * minimum once. So a quote for many units can be well under the per-unit
 * quote times that many. `snapshotId` is the trailing VWAP snapshot an
 * issuance-currency payment must name (zero on the reference rail). It goes
 * stale at the next hourly cutoff.
 */
export async function quoteSettlement(
  n: Network,
  series: Hex,
  token: `0x${string}`,
  units: bigint,
): Promise<{ settlementCurrency: number; paymentMinor: bigint; snapshotId: bigint }> {
  const [settlementCurrency, paymentMinor, snapshotId] = (await n.client.readContract({
    address: addr(n, "factory"),
    abi: FACTORY_ABI,
    functionName: "quoteSettlement",
    args: [series, token, units],
  })) as [number, bigint, bigint];
  return { settlementCurrency: Number(settlementCurrency), paymentMinor, snapshotId };
}

export const auctionStageOf = (n: Network, worldwideDay: number) =>
  n.client.readContract({
    address: addr(n, "auction"),
    abi: AUCTION_ABI,
    functionName: "getAuctionStage",
    args: [worldwideDay],
  }) as Promise<number>;

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

export async function bridgeSendParam(n: Network, series: Hex, units: bigint, recipient: Address) {
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
    units,
  };
}
