import { type Address, getAddress } from "viem";
import { loadConfig } from "../config.js";
import IDesis from "../abi/generated/precompiles/IDesis.js";
import IIntex from "../abi/generated/precompiles/IIntex.js";
import IIntexFactory from "../abi/generated/precompiles/IIntexFactory.js";
import IVaultRouter from "../abi/generated/precompiles/IVaultRouter.js";
import EscrowAdapter from "../abi/generated/intex/EscrowAdapter.js";
import IntexAuction from "../abi/generated/intex/IntexAuction.js";
import IIntexNFT1155 from "../abi/generated/intex/IIntexNFT1155.js";
import IIntexNFT1155Bridge from "../abi/generated/intex/IIntexNFT1155Bridge.js";
import IOriginRouter from "../abi/generated/intex/IOriginRouter.js";

/**
 * Addresses + ABIs for the Intex tools (auction commit/reveal, escrow, NFT,
 * series registry, cross-chain bridge, settlement/Promis).
 *
 * Intex is cross-chain. The auction + escrow + NFT run on the target chains the
 * origin router serves. The series ledger (Intex), settlement (IntexFactory) and
 * Promis live on outbe as runtime precompiles. Addresses are embedded constants.
 * The build inlines the ABI JSON. This module never reads it at runtime.
 *
 * ABIs are generated from Solidity (contracts/{intex,precompiles,tokens}), never
 * hand-written. This matches the convention in src/registry.ts. Where a method is
 * only on the concrete contract and not on its interface, this module uses the
 * concrete artifact.
 */

/** The Intex contracts the tools address. */
export interface IntexAddresses {
  auction?: Address;
  escrow?: Address;
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

// The app contracts are CREATE3 proxies (salt "outbe-intex:<Name>:v5.0.0"), so
// each one shares a single address on every chain. Which chains run them is the
// origin router's target list; the payment token is the escrow's own.
const APP = {
  auction: a("0x66F0377e4dCbf4df50134eAf147b2B305AE86813"),
  escrow: a("0x917dBD5EeEEc541BB25397F8d38D266d2E88ae19"),
  nft: a("0x769C2fe8f14d28527eEfFc93d84FD4cC1BfbabcA"),
  nftBridge: a("0x298b8d10480f18fbE6FB220D59EE8ad6159904A9"),
};

/** outbe runtime precompiles (addresses.rs) + the fan-out router. */
const OUTBE_ONLY = {
  intex: a("0x0000000000000000000000000000000000001014"),
  factory: a("0x0000000000000000000000000000000000001015"),
  promis: a("0x0000000000000000000000000000000000001337"),
  desis: a("0x0000000000000000000000000000000000001016"),
  vaultRouter: a("0x0000000000000000000000000000000000001017"),
  // CREATE3 proxy, salt "outbe-intex:OriginRouter:v5.0.0".
  originRouter: a("0x3439ebc6732ECA3F11125F4c1160C1cdd2C47Dc8"),
};

/** Block the NFT pair was deployed at, per network. The tools read holdings from transfer logs,
 *  and the scan starts here. An unset network scans from genesis, which public RPCs range-limit.
 *  Fill this in when the pair is deployed. Recovering a deployment block afterwards needs archive
 *  state. */
const NFT_DEPLOY_BLOCK: Record<string, bigint> = {};

/** First block worth scanning for this network's NFT transfer logs. */
export function intexNftFromBlock(network: string): bigint {
  return NFT_DEPLOY_BLOCK[network] ?? 0n;
}

/** What the address book needs to know about a network. */
export interface IntexChain {
  name: string;
  isOutbe: boolean;
}

/** An Intex contract the network has no address for. */
export class NotConfiguredError extends Error {}

/** Resolve a contract address for a network, or throw a clear error. */
export function intexAddress(network: IntexChain, key: keyof IntexAddresses): Address {
  let addr: Address | undefined;
  if (key in APP) {
    addr = APP[key as keyof typeof APP];
  } else if (network.isOutbe) {
    const override = key === "originRouter" ? loadConfig().intexOriginRouter : undefined;
    addr = override ? getAddress(override) : OUTBE_ONLY[key as keyof typeof OUTBE_ONLY];
  }
  if (!addr) {
    throw new NotConfiguredError(`Intex "${key}" is not configured on "${network.name}"`);
  }
  return addr;
}

// --- ABIs ------------------------------------------------------------------

/** IntexAuction (target chains): commit/reveal + auction views. */
export const AUCTION_ABI = IntexAuction;

/** IntexNFT1155 (target chains + outbe): holder-facing reads. */
export const NFT_ABI = IIntexNFT1155;

/** Intex (outbe precompile): canonical cross-chain series ledger. */
export const INTEX_ABI = IIntex;

/** IntexNFT1155Bridge: the cross-chain NFT bridge between Intex chains over ERC-7786. */
export const NFT_BRIDGE_ABI = IIntexNFT1155Bridge;

/** IntexFactory (outbe precompile): holder-facing settlement + Promis mining. */
export const FACTORY_ABI = IIntexFactory;

/** Desis (outbe precompile): auction stage + per-chain bid fan-in views. */
export const DESIS_ABI = IDesis;

/** OriginRouter (outbe): the auction's target-chain registry + per-day snapshot. */
export const ORIGIN_ROUTER_ABI = IOriginRouter;

/** EscrowAdapter (target chains): bid locks, commit bonds and refunds. */
export const ESCROW_ABI = EscrowAdapter;

/** VaultRouter (outbe precompile): the reserve asset registry. */
export const VAULT_ROUTER_ABI = IVaultRouter;

export { ERC20_ABI } from "../net/erc20.js";
