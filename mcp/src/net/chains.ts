import type { Chain } from "viem";

export interface NetworkDef {
  name: string;
  chainId: number;
  rpc: string;
}

/** Chains the Intex and intent tools reach by name or chain id. */
export const NETWORKS: NetworkDef[] = [
  { name: "bsc-testnet", chainId: 97, rpc: "https://bsc-testnet-rpc.publicnode.com" },
  { name: "outbe-testnet", chainId: 54322345, rpc: "https://rpc.testnet.outbe.net" },
];

export const OUTBE_NETWORK = "outbe-testnet";
export const TARGET_NETWORK = "bsc-testnet";

export function nativeCurrencyForChainId(id: number): Chain["nativeCurrency"] {
  if (id === 424_242 || id === 54_322_345) {
    return { name: "COEN", symbol: "COEN", decimals: 18 };
  }
  if (id === 56 || id === 97) {
    return { name: "BNB", symbol: "BNB", decimals: 18 };
  }
  // External EVM/LZ domains retain the pre-cutover 18-decimal native boundary.
  return { name: "Ether", symbol: "ETH", decimals: 18 };
}
