import { type Address, getAddress, zeroAddress } from "viem";

/**
 * Token registry for intent tools. A logical symbol maps to a per-chain token
 * address (addresses are network-specific, so the key is the chain id):
 *
 *   USD  -> USDT0 OFT (outbe) / USDT (BSC)
 *   COEN -> native (outbe)    / wCOEN (BSC)
 *
 * A raw 0x address is always accepted too. Decimals are read on-chain elsewhere.
 */

/** Symbol → { chainId → address }. Chain ids match the NETWORKS table. */
const TOKENS: Record<string, Record<number, Address>> = {
  USD: {
    54322345: getAddress("0x1a5FF18C7A3B9D6f9F2640d9e6CF074ee80d71fa"), // USDT0 OFT (outbe testnet)
    97: getAddress("0x78366397b72D0c283658DA5A38C450455A97e595"), // USDT (BSC testnet)
  },
  COEN: {
    54322345: zeroAddress, // native COEN (outbe testnet)
    97: getAddress("0xC3091D1ed358B85B4a1Fd9279D40316479445fD7"), // wCOEN (BSC testnet)
  },
};

/**
 * Real ticker → logical symbol. Unlike network names (which the model normalizes
 * itself), token tickers need this map: the model knows "USDT"/"USDT0"/"wCOEN"
 * but not that they share one logical entry per asset across networks.
 */
const TOKEN_ALIASES: Record<string, string> = {
  USDT: "USD",
  USDT0: "USD",
  WCOEN: "COEN",
};

export interface TokenRef {
  address: Address;
  /** logical symbol when resolved from the registry, else the raw address */
  symbol: string;
}

/** Logical symbol for a known token address on a chain, or undefined. */
export function symbolForAddress(addr: Address, chainId: number): string | undefined {
  for (const [sym, perChain] of Object.entries(TOKENS)) {
    const a = perChain[chainId];
    if (a !== undefined && getAddress(a) === addr) return sym;
  }
  return undefined;
}

/** Resolve a token spec (symbol or 0x address) to an address on a network. */
export function resolveToken(spec: string, net: { chainId: number; name: string }): TokenRef {
  const s = spec.trim();
  if (/^0x[0-9a-fA-F]{40}$/.test(s)) {
    const address = getAddress(s);
    return { address, symbol: symbolForAddress(address, net.chainId) ?? address };
  }
  const key = TOKEN_ALIASES[s.toUpperCase()] ?? s.toUpperCase();
  const entry = TOKENS[key];
  if (!entry) {
    const known = [...Object.keys(TOKENS), ...Object.keys(TOKEN_ALIASES)].join(", ");
    throw new Error(`unknown token "${spec}"; known: ${known}, or a 0x address`);
  }
  const address = entry[net.chainId];
  if (address === undefined) {
    throw new Error(`token ${key} is not available on ${net.name} (chainId ${net.chainId})`);
  }
  return { address, symbol: key };
}
