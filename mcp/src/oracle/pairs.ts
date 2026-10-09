import type { Ctx } from "../chain.js";
import { view } from "../read.js";
import { rememberMarkets } from "./markets.js";

/** Every Oracle pair with the decimals each side is quoted in and whether it is voted on. */
export async function pairTable(ctx: Ctx) {
  // The oracle enumerates its registry by index rather than returning the
  // whole table, so it is assembled here.
  const count = Number(await view(ctx, "oracle", "getPairCount", []));
  const indices = Array.from({ length: count }, (_, i) => i + 1);
  const pairs = await Promise.all(
    indices.map(async (index) => {
      const pair = (await view(ctx, "oracle", "getPairByIndex", [index])) as {
        base: string;
        quote: string;
        baseScale: number;
        quoteScale: number;
      };
      const active = await view(ctx, "oracle", "isVoteTarget", [pair.base, pair.quote]);
      return {
        index,
        base: pair.base,
        quote: pair.quote,
        baseScale: Number(pair.baseScale),
        quoteScale: Number(pair.quoteScale),
        active,
      };
    }),
  );
  rememberMarkets(
    ctx.chain.id,
    pairs.map((p) => [p.base, p.quote, p.baseScale, p.quoteScale] as const),
  );
  return pairs;
}
