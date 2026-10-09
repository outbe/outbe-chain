import { type Ctx, readView } from "./chain.js";
import { humanizeReturn, type ReturnFormatContext } from "./format.js";
import { knownMarkets, scaleOf } from "./oracle/markets.js";
import { CONTRACTS, resolveContract } from "./registry.js";

/** Read a view method and return the humanized result object. */
export async function view(
  ctx: Ctx,
  contract: string,
  method: string,
  args: unknown[] = [],
): Promise<unknown> {
  const entry = resolveContract(contract);
  const { fn, result } = await readView(ctx, entry, method, args);
  const oracle = resolveContract("oracle");
  const contractName = Object.entries(CONTRACTS).find(
    ([, candidate]) => candidate.address === entry.address,
  )?.[0];
  const formatContext: ReturnFormatContext = { contractName };
  if (entry.address === oracle.address) {
    const markets = knownMarkets(ctx.chain.id);
    formatContext.oracleArgs = args;
    formatContext.scaleFor = (base: unknown, quote: unknown) => scaleOf(markets, base, quote);
  }
  return humanizeReturn(fn, result, formatContext);
}

/** A view's raw `uint` result as a number: counts drive loops, so they skip formatting. */
export async function readCount(ctx: Ctx, contract: string, method: string, args: unknown[] = []): Promise<number> {
  const { result } = await readView(ctx, resolveContract(contract), method, args);
  return Number(result as bigint);
}

/** Token ids `owner` holds on an enumerable NFT precompile, read in parallel. */
export async function ownedTokenIds(ctx: Ctx, contract: string, owner: string): Promise<bigint[]> {
  const entry = resolveContract(contract);
  const balance = await readCount(ctx, contract, "balanceOf", [owner]);
  return Promise.all(
    Array.from({ length: balance }, async (_, index) => {
      const { result } = await readView(ctx, entry, "tokenOfOwnerByIndex", [owner, index]);
      return result as bigint;
    }),
  );
}
