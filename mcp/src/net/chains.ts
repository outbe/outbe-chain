import type { Chain } from "viem";

export interface KnownChain {
  name: string;
  chainId: number;
  /** A public RPC; an Outbe chain has none here because it is the node the server is connected to. */
  rpc?: string;
}

export const KNOWN_CHAINS: KnownChain[] = [
  { name: "outbe-testnet", chainId: 54_322_345 },
  { name: "bsc-testnet", chainId: 97, rpc: "https://bsc-testnet-rpc.publicnode.com" },
  { name: "sepolia", chainId: 11_155_111, rpc: "https://ethereum-sepolia-rpc.publicnode.com" },
];

/** The Outbe chain the server is connected to, whatever its chain id. */
export const OUTBE_NETWORK = "outbe";
export const TARGET_NETWORK = "bsc-testnet";

export const NETWORK_NAMES = [OUTBE_NETWORK, ...KNOWN_CHAINS.map((c) => c.name)];

export function chainName(chainId: number): string | undefined {
  return KNOWN_CHAINS.find((c) => c.chainId === chainId)?.name;
}

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
