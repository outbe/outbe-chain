// Shared LayerZero constants and utilities.
// Single source of truth for LZ infrastructure addresses, chain definitions,
// EIDs, and common helpers used across tasks and scripts.

import { createPublicClient, defineChain, http } from "viem";

// =============================================================================
// Outbe Chain Definitions (hardhat-viem doesn't know chain IDs 424242/512512)
// =============================================================================

export const OUTBE_CHAINS = {
  outbeDevnet: defineChain({
    id: 424242,
    name: "Outbe Dev",
    nativeCurrency: { decimals: 18, name: "COEN", symbol: "COEN" },
    rpcUrls: { default: { http: ["https://eth.d.outbe.net"] } },
  }),
  outbePrivnet: defineChain({
    id: 512512,
    name: "Outbe Priv",
    nativeCurrency: { decimals: 18, name: "COEN", symbol: "COEN" },
    rpcUrls: { default: { http: ["https://eth.p.outbe.net"] } },
  }),
  outbeTestnet: defineChain({
    id: 512215,
    name: "Outbe Testnet",
    nativeCurrency: { decimals: 18, name: "COEN", symbol: "COEN" },
    rpcUrls: { default: { http: ["https://eth.testnet.outbe.net"] } },
  }),
  outbeTestnetNew: defineChain({
    id: 54322345,
    name: "Outbe Testnet New",
    nativeCurrency: { decimals: 18, name: "COEN", symbol: "COEN" },
    rpcUrls: { default: { http: ["https://rpc.testnet.outbe.net"] } },
  }),
} as const;

// =============================================================================
// LZ V2 Infrastructure (identical across all networks in this custom deployment)
// =============================================================================

export const LZ_INFRA = {
  endpoint: "0x2915f5C5835576CC6E4bFBc002519E2B37cCABe2" as `0x${string}`,
  sendUln302: "0x5669B63eB734b749D1e15793642496b3ad6d58a0" as `0x${string}`,
  receiveUln302: "0x7369759D0a8f54015d5899006Ef1bE593769fd7d" as `0x${string}`,
  dvn: "0x268BFe4feac742721146029EAbA3BEf5fBd1B51F" as `0x${string}`,
  executor: "0xf5C9d8ddE889fBaE256D57c28dFCC4599032e1fc" as `0x${string}`,
  confirmations: 1n,
  maxMessageSize: 10000,
};

// =============================================================================
// Network → Chain ID / LZ Endpoint ID
// =============================================================================

export const NETWORK_CHAIN_IDS: Record<string, number> = {
  bscTestnet: 97,
  bsc: 56,
  outbePrivnet: 512512,
  outbeDevnet: 424242,
  outbeTestnet: 512215,
  outbeTestnetNew: 54322345,
};

export const NETWORK_TO_EID: Record<string, number> = {
  outbeDevnet: 40712,
  outbePrivnet: 40512,
  outbeTestnet: 40812,
  outbeTestnetNew: 40912,
  bscTestnet: 40102,
  bsc: 30102,
};

export const CHAIN_ID_TO_EID: Record<number, number> = {
  424242: 40712,
  512512: 40512,
  512215: 40812,
  54322345: 40912,
  97: 40102,
  56: 30102,
};

// =============================================================================
// Environment Helpers
// =============================================================================

export function getEnvRpcAndPk(networkName: string): { rpc: string; pk: string } {
  switch (networkName) {
    case "outbeDevnet":
      return { rpc: process.env.OUTBE_RPC_URL ?? "https://eth.d.outbe.net", pk: process.env.OUTBE_PRIVATE_KEY ?? "" };
    case "outbePrivnet":
      return { rpc: process.env.OUTBE_RPC_URL ?? "https://eth.p.outbe.net", pk: process.env.OUTBE_PRIVATE_KEY ?? "" };
    case "outbeTestnet":
      return { rpc: process.env.OUTBE_RPC_URL ?? "https://eth.testnet.outbe.net", pk: process.env.OUTBE_PRIVATE_KEY ?? "" };
    case "outbeTestnetNew":
      return { rpc: process.env.OUTBE_RPC_URL ?? "https://rpc.testnet.outbe.net", pk: process.env.OUTBE_PRIVATE_KEY ?? "" };
    case "bscTestnet":
      return { rpc: process.env.BSC_TESTNET_RPC_URL ?? "https://bsc-testnet.publicnode.com", pk: process.env.BSC_TESTNET_PRIVATE_KEY ?? "" };
    case "bsc":
      return { rpc: process.env.BSC_MAINNET_RPC_URL ?? "https://bsc-dataseed1.binance.org", pk: process.env.BSC_MAINNET_PRIVATE_KEY ?? "" };
    default:
      throw new Error(`Unknown network: ${networkName}`);
  }
}

export function makeChain(networkName: string, rpc?: string) {
  if (networkName in OUTBE_CHAINS) return OUTBE_CHAINS[networkName as keyof typeof OUTBE_CHAINS];
  const chainId = NETWORK_CHAIN_IDS[networkName];
  if (!chainId) throw new Error(`Unknown network: ${networkName}`);
  const effectiveRpc = rpc ?? getEnvRpcAndPk(networkName).rpc;
  return defineChain({
    id: chainId,
    name: networkName,
    nativeCurrency: { decimals: 18, name: "ETH", symbol: "ETH" },
    rpcUrls: { default: { http: [effectiveRpc] } },
  });
}

export function makePublicClient(networkName: string) {
  const { rpc } = getEnvRpcAndPk(networkName);
  const chain = makeChain(networkName, rpc);
  return createPublicClient({ chain, transport: http(rpc) });
}

// =============================================================================
// PacketV1 Parsing
// =============================================================================

export const PACKET_SENT_TOPIC = "0x1ab700d4ced0c005b164c0f789fd09fcbb0156d4c2041b8a3bfbcd961cd1567f";

export interface PacketV1 {
  nonce: bigint;
  srcEid: number;
  sender: `0x${string}`;
  dstEid: number;
  receiver: `0x${string}`;
  guid: `0x${string}`;
  message: `0x${string}`;
}

export function parsePacketV1(encoded: `0x${string}`): PacketV1 {
  const hex = encoded.slice(2);
  let offset = 0;
  const read = (bytes: number) => {
    const slice = hex.slice(offset, offset + bytes * 2);
    offset += bytes * 2;
    return slice;
  };

  read(1); // version
  const nonce = BigInt("0x" + read(8));
  const srcEid = Number("0x" + read(4));
  const sender = ("0x" + read(32)) as `0x${string}`;
  const dstEid = Number("0x" + read(4));
  const receiver = ("0x" + read(32)) as `0x${string}`;
  const guid = ("0x" + read(32)) as `0x${string}`;
  const message = ("0x" + hex.slice(offset)) as `0x${string}`;

  return { nonce, srcEid, sender, dstEid, receiver, guid, message };
}

// =============================================================================
// Nonce ABIs (for delivery waiting and nonce checking)
// =============================================================================

export const ENDPOINT_NONCE_ABI = [
  { inputs: [{ name: "_sender", type: "address" }, { name: "_dstEid", type: "uint32" }, { name: "_receiver", type: "bytes32" }], name: "outboundNonce", outputs: [{ type: "uint64" }], stateMutability: "view", type: "function" },
  { inputs: [{ name: "_receiver", type: "address" }, { name: "_srcEid", type: "uint32" }, { name: "_sender", type: "bytes32" }], name: "lazyInboundNonce", outputs: [{ type: "uint64" }], stateMutability: "view", type: "function" },
  { inputs: [{ name: "_receiver", type: "address" }, { name: "_srcEid", type: "uint32" }, { name: "_sender", type: "bytes32" }, { name: "_nonce", type: "uint64" }], name: "inboundPayloadHash", outputs: [{ type: "bytes32" }], stateMutability: "view", type: "function" },
] as const;

// =============================================================================
// Utility: address ↔ bytes32
// =============================================================================

export function addressToBytes32(addr: `0x${string}`): `0x${string}` {
  return ("0x" + addr.slice(2).toLowerCase().padStart(64, "0")) as `0x${string}`;
}

export function bytes32ToAddress(b32: `0x${string}`): `0x${string}` {
  return ("0x" + b32.slice(-40)) as `0x${string}`;
}
