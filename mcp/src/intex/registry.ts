import { type Abi, type Address, getAddress } from "viem";
import IDesisJson from "../../../contracts/precompiles/abi-export/IDesis.json";
import IIntexJson from "../../../contracts/precompiles/abi-export/IIntex.json";
import IIntexFactoryJson from "../../../contracts/precompiles/abi-export/IIntexFactory.json";
import IVaultRouterJson from "../../../contracts/precompiles/abi-export/IVaultRouter.json";
import EscrowAdapterJson from "../../../contracts/intex/abi-export/EscrowAdapter.json";
import IntexAuctionJson from "../../../contracts/intex/abi-export/IntexAuction.json";
import IIntexNFT1155Json from "../../../contracts/intex/abi-export/IIntexNFT1155.json";
import IIntexNFT1155BridgeJson from "../../../contracts/intex/abi-export/IIntexNFT1155Bridge.json";
import IOriginRouterJson from "../../../contracts/intex/abi-export/IOriginRouter.json";
import IERC20Json from "../../../contracts/tokens/abi-export/IERC20.json";

/** contracts/intex exports as `{ contractName, abi }`; the others as a bare array. */
const abiOf = (json: unknown): Abi =>
  (Array.isArray(json) ? json : (json as { abi: unknown }).abi) as Abi;

/**
 * Addresses + ABIs for the Intex tools (auction commit/reveal, escrow, NFT,
 * series registry, cross-chain bridge, settlement/Promis).
 *
 * Intex is cross-chain: the auction + escrow + NFT run on target chains (BSC
 * today, more later); the series ledger (Intex), settlement
 * (IntexFactory) and Promis live on outbe as runtime precompiles. Addresses are
 * embedded constants, keyed by network so a new target chain is an added branch,
 * not a rewrite. The ABI JSON is inlined at build time, never read at runtime.
 *
 * ABIs are generated from Solidity (contracts/{intex,precompiles,tokens}), never
 * hand-written - matching the convention in src/registry.ts. Where a method is
 * only on the concrete contract and not its interface, the concrete artifact is
 * used.
 */

export interface NetworkDef {
  name: string;
  chainId: number;
  rpc: string;
}

/** Supported networks. `outbe-testnet` reuses the connected ctx when ids match. */
export const NETWORKS: NetworkDef[] = [
  { name: "bsc-testnet", chainId: 97, rpc: "https://bsc-testnet-rpc.publicnode.com" },
  { name: "outbe-testnet", chainId: 54322345, rpc: "https://rpc.testnet.outbe.net" },
];

/** Per-network Intex contract addresses. Empty until deployed on that network. */
export interface IntexAddresses {
  auction?: Address;
  escrow?: Address;
  paymentToken?: Address;
  nft?: Address;
  nftBridge?: Address;
  intex?: Address;
  factory?: Address;
  promis?: Address;
  desis?: Address;
  vaultRouter?: Address;
  originRouter?: Address;
}

const a = (s: string): Address => getAddress(s);

const OUTBE = "outbe-testnet";

// The app contracts are CREATE3 proxies (salt "outbe-intex:<Name>:v3.0.0"), so
// each one shares a single address on every chain; only the wCOEN payment token
// is a per-chain deployment. Networks gate availability, addresses do not.
const APP = {
  auction: a("0x3EB42c05F1bF00CdAaF9F825b2DDA3fc93D20d7f"),
  escrow: a("0x88C521C9A79b16a00aa2e4285670246cC2dbe868"),
  nft: a("0x4b25C6af5C45240D9cE546AF223Dd7132C7E39Af"),
  nftBridge: a("0x4056B412a0D3A0931FE2078Be8Ec4a57ADDb3958"),
};

/** outbe runtime precompiles (addresses.rs) + the fan-out router. */
const OUTBE_ONLY = {
  intex: a("0x0000000000000000000000000000000000001014"),
  factory: a("0x0000000000000000000000000000000000001015"),
  promis: a("0x0000000000000000000000000000000000001337"),
  desis: a("0x0000000000000000000000000000000000001016"),
  vaultRouter: a("0x0000000000000000000000000000000000001017"),
  // CREATE3 proxy, salt "outbe-intex:OriginRouter:v3.0.0".
  originRouter: a("0xCBfa290DCd34319Ff1aec79A4084f2C900977599"),
};

/** Networks where the auction/escrow pair is live. The NFT pair runs on the origin
 *  and every target; enabling a new target = adding it here + its wCOEN below. */
const AUCTION_LIVE = new Set(["bsc-testnet"]);

/** WCOEN - the auction's six-decimal payment token, per chain. */
const PAYMENT_TOKEN: Record<string, Address> = {
  "bsc-testnet": a("0x2FCC92D751086AFeECEaE0f3AC133B27E8F0D57c"),
};

/** Resolve a contract address for a network, or throw a clear error. */
export function intexAddress(network: string, key: keyof IntexAddresses): Address {
  let addr: Address | undefined;
  switch (key) {
    case "auction":
    case "escrow":
      addr = AUCTION_LIVE.has(network) ? APP[key] : undefined;
      break;
    case "nft":
    case "nftBridge":
      addr = network === OUTBE || AUCTION_LIVE.has(network) ? APP[key] : undefined;
      break;
    case "paymentToken":
      addr = PAYMENT_TOKEN[network];
      break;
    default:
      addr = network === OUTBE ? OUTBE_ONLY[key] : undefined;
  }
  if (!addr) {
    throw new Error(`Intex "${key}" is not configured on "${network}"`);
  }
  return addr;
}

/** Destination EVM chain id of each network's bridge counterpart (NFT destination). */
export const BRIDGE_DST_CHAIN_ID: Record<string, number> = {
  "bsc-testnet": 54322345, // -> outbe-testnet
  "outbe-testnet": 97, // -> bsc-testnet
};

/** Destination chain id for bridging an NFT out of a network, or throw. */
export function bridgeDstChainId(network: string): number {
  const chainId = BRIDGE_DST_CHAIN_ID[network];
  if (chainId === undefined) {
    throw new Error(`Intex bridge destination chain id is not configured on "${network}"`);
  }
  return chainId;
}

// --- ABIs ------------------------------------------------------------------

/** IntexAuction (BSC): commit/reveal + auction views. */
export const AUCTION_ABI: Abi = abiOf(IntexAuctionJson);

/** IntexNFT1155 (BSC + outbe): holder-facing reads. */
export const NFT_ABI: Abi = abiOf(IIntexNFT1155Json);

/** Intex (outbe precompile): canonical cross-chain series ledger. */
export const INTEX_ABI: Abi = abiOf(IIntexJson);

/** IntexNFT1155Bridge: the cross-chain NFT bridge (BSC <-> outbe) over ERC-7786. */
export const NFT_BRIDGE_ABI: Abi = abiOf(IIntexNFT1155BridgeJson);

/** IntexFactory (outbe precompile): holder-facing settlement + Promis mining. */
export const FACTORY_ABI: Abi = abiOf(IIntexFactoryJson);

/** Desis (outbe precompile): auction stage + per-chain bid fan-in views. */
export const DESIS_ABI: Abi = abiOf(IDesisJson);

/** OriginRouter (outbe): the auction's target-chain registry + per-day snapshot. */
export const ORIGIN_ROUTER_ABI: Abi = abiOf(IOriginRouterJson);

/** EscrowAdapter (target chains): bid locks, commit bonds and refunds. */
export const ESCROW_ABI: Abi = abiOf(EscrowAdapterJson);

/** VaultRouter (outbe precompile): the reserve asset registry. */
export const VAULT_ROUTER_ABI: Abi = abiOf(IVaultRouterJson);

/** ERC20 (BSC payment token; outbe Promis balance). */
export const ERC20_ABI: Abi = abiOf(IERC20Json);
