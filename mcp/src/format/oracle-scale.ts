import { type AbiFunction, getAddress, zeroAddress } from "viem";

/** One decimal scale for the whole result, or one per element of a `bases`/`quotes` table. */
export type DecimalScale = number | (number | undefined)[];

export interface ReturnFormatContext {
  /** Canonical registry key of the precompile whose result is being formatted. */
  contractName?: string;
  /** Raw ABI arguments for a call resolved to the Oracle precompile. */
  oracleArgs?: readonly unknown[];
  /** The registry's own answer for a market. The local rule is the fallback. */
  scaleFor?: (base: unknown, quote: unknown) => number | undefined;
}

/** Oracle reads quoted at 1e6 whatever their market. */
const SIX_DECIMAL_ORACLE_READS = new Set(["getCoenExchangeRateFor", "getPolicyRate"]);

function isIsoCurrencyAddress(value: unknown): boolean {
  if (typeof value !== "string") return false;
  try {
    const raw = BigInt(getAddress(value));
    if (raw < 0xcc000n || raw > 0xcc999n) return false;
    const packed = Number(raw - 0xcc000n);
    return [0, 4, 8].every((shift) => ((packed >> shift) & 0xf) <= 9);
  } catch {
    return false;
  }
}

function isCoen(value: unknown): boolean {
  if (typeof value !== "string") return false;
  try {
    return getAddress(value) === zeroAddress;
  } catch {
    return false;
  }
}

/** Decimal scale override for a stablecoin-backed COEN/ISO Oracle market. */
function coenIsoMarketDecimals(base: unknown, quote: unknown): 6 | undefined {
  const coenIso = isCoen(base) && isIsoCurrencyAddress(quote);
  const isoCoen = isIsoCurrencyAddress(base) && isCoen(quote);
  return coenIso || isoCoen ? 6 : undefined;
}

/** The scale of every element of a `bases`/`quotes` table, when the result is one. */
function tableScale(
  fn: AbiFunction,
  result: unknown,
  scaleFor: (base: unknown, quote: unknown) => number | undefined,
): DecimalScale | undefined {
  const outputs = fn.outputs ?? [];
  const basesIndex = outputs.findIndex((output) => output.name === "bases");
  const quotesIndex = outputs.findIndex((output) => output.name === "quotes");
  if (basesIndex < 0 || quotesIndex < 0 || !Array.isArray(result)) return undefined;
  const bases = result[basesIndex];
  const quotes = result[quotesIndex];
  if (!Array.isArray(bases) || !Array.isArray(quotes) || bases.length !== quotes.length) {
    return undefined;
  }
  return bases.map((base, index) => scaleFor(base, quotes[index]));
}

/** The decimal scale an Oracle read is presented in, when its market fixes one. */
export function oraclePresentationScale(
  fn: AbiFunction,
  result: unknown,
  context: ReturnFormatContext | undefined,
): DecimalScale | undefined {
  if (!context) return undefined;
  if (SIX_DECIMAL_ORACLE_READS.has(fn.name)) return 6;
  const scaleFor = (base: unknown, quote: unknown) =>
    context.scaleFor?.(base, quote) ?? coenIsoMarketDecimals(base, quote);
  const inputs = fn.inputs ?? [];
  if (inputs[0]?.name === "base" && inputs[1]?.name === "quote") {
    return scaleFor(context.oracleArgs?.[0], context.oracleArgs?.[1]);
  }
  return tableScale(fn, result, scaleFor);
}
