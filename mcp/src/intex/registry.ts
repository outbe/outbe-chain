import { type Abi, type Address, getAddress, parseAbi } from "viem";

/**
 * Addresses + ABIs for the Intex tools (auction commit/reveal, escrow, NFT,
 * series registry, cross-chain bridge, settlement/Promis).
 *
 * Intex is cross-chain: the auction + escrow + NFT run on target chains (BSC
 * today, more later); the series ledger (Intex), settlement
 * (IntexFactory) and Promis live on outbe as runtime precompiles. Addresses are
 * embedded constants, keyed by network so a new target chain is an added branch,
 * not a rewrite. The MCP never reads JSON at runtime.
 *
 * ABIs are embedded as viem human-readable signatures (no Solidity compile step),
 * matching the convention in src/registry.ts and src/intent/registry.ts.
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
}

const a = (s: string): Address => getAddress(s);

// CREATE3 proxies (salt "outbe-intex:<Name>:v2.0.0"), so app contracts share an
// address across chains.
export const INTEX: Record<string, IntexAddresses> = {
  "bsc-testnet": {
    auction: a("0xCf7c1b2107a0025a6ce82442473Cb4f7A8dF2E0b"),
    escrow: a("0x9eC00F7e603f56d5eDfc866aF7CC9E6f6Fa26A8E"),
    // wCOEN — auction escrow pays wrapped COEN (18 decimals).
    paymentToken: a("0x2FCC92D751086AFeECEaE0f3AC133B27E8F0D57c"),
    nft: a("0x4Ccbc413a5f159Da316178F8b7576C923b4D1e5d"),
    nftBridge: a("0xD905a9Af95330d9725Cf060f6A89Ef48FB4A7Dfc"),
  },
  "outbe-testnet": {
    nft: a("0x4Ccbc413a5f159Da316178F8b7576C923b4D1e5d"),
    nftBridge: a("0xD905a9Af95330d9725Cf060f6A89Ef48FB4A7Dfc"),
    // outbe runtime precompiles (addresses.rs):
    intex: a("0x0000000000000000000000000000000000001014"),
    factory: a("0x0000000000000000000000000000000000001015"),
    promis: a("0x0000000000000000000000000000000000001337"),
  },
};

/** Resolve a contract address for a network, or throw a clear error. */
export function intexAddress(network: string, key: keyof IntexAddresses): Address {
  const addr = INTEX[network]?.[key];
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
export const AUCTION_ABI: Abi = parseAbi([
  "function commitBid(uint32 worldwideDay, bytes32 commitHash)",
  "function revealBid(uint32 worldwideDay, uint16 quantity, uint32 bidRate, uint64 chainId, bytes signature)",
  "function cancelCommit(uint32 worldwideDay)",
  "function claimCommitBond(uint32 worldwideDay, address bidder)",
  "function getAuctionStage(uint32 worldwideDay) view returns (uint8)",
  "function getAuctionInfo(uint32 worldwideDay) view returns ((uint8 worldwideDayState, (uint32 commitEnd, uint32 revealEnd, uint32 issuanceEnd) schedule, (uint16 issuanceCurrency, uint16 referenceCurrency, uint128 promisLoadMinor, (uint16 windowDays, uint16 thresholdDays, uint32 intexCallPeriod) callTrigger, uint32 minIntexBidRate, uint16 minIntexBidQuantity, uint64 entryPriceMinor, uint64 floorPriceMinor, uint64 callPriceMinor, uint128 commitBondMinor) params, (uint64 auctionClearingRate, uint32 wonBidsCount, uint32 issuedIntexCount, uint128 issuedIntexLoadedPromis) result) auctionData)",
  "function committedBidsByHash(uint32 worldwideDay, address bidder) view returns (bytes32)",
  "function revealedBidsByBidder(uint32 worldwideDay, address bidder) view returns (bool)",
  "function escrowContract() view returns (address)",
  "event AuctionStageUpdated(uint32 indexed worldwideDay, uint8 auctionStage, uint32 timestamp, string reason)",
]);

/** IntexNFT1155 (BSC + outbe): holder-facing reads. */
export const NFT_ABI: Abi = parseAbi([
  "function getOwnedSeriesWithBalances(address owner) view returns (uint256[] ownedTokenIds, uint256[] balances)",
  "function getAuctionWonCount(uint32 worldwideDay, address account) view returns (uint16)",
  "function statusOf(uint256 tokenId) view returns (uint8)",
  "function balanceOf(address account, uint256 id) view returns (uint256)",
  "function tokenIds(uint32 seriesId) view returns (uint256 issued, uint256 settled)",
  "function readData(uint32 seriesId) view returns ((uint16 issuanceCurrency, uint16 referenceCurrency, uint32 issuedIntexCount, uint128 promisLoadMinor, uint64 entryPriceMinor, uint64 floorPriceMinor, uint64 callPriceMinor, (uint16 windowDays, uint16 thresholdDays, uint32 intexCallPeriod) callTrigger, uint32 issuedAt, uint32 calledAt, uint32 totalSupply, uint8 status, uint8 state) data)",
  "function isApprovedForAll(address account, address operator) view returns (bool)",
  "function setApprovalForAll(address operator, bool approved)",
]);

/** Intex (outbe precompile): canonical cross-chain series ledger. */
export const INTEX_ABI: Abi = parseAbi([
  "function seriesData(uint32 seriesId) view returns ((uint32 seriesId, uint256 promisLoadMinor, uint256 entryPriceMinor, uint256 floorPriceMinor, uint32 issuedIntexCount, uint16 callWindowDays, uint16 callThresholdDays, uint256 callPriceMinor, uint8 state, uint32 issuedAt, uint32 calledAt, uint32 intexCallPeriod, uint16 issuanceCurrency, uint16 referenceCurrency) data)",
  "function seriesExists(uint32 seriesId) view returns (bool)",
  "function totalSeries() view returns (uint64)",
  "function seriesAt(uint64 index) view returns (uint32)",
]);

/** IntexNFT1155Bridge: the cross-chain NFT bridge (BSC <-> outbe) over ERC-7786. */
export const NFT_BRIDGE_ABI: Abi = parseAbi([
  "function quoteSend((uint32 dstChainId, bytes32 to, uint256 tokenId, uint256 amount) sendParam) view returns (uint256 fee)",
  "function send((uint32 dstChainId, bytes32 to, uint256 tokenId, uint256 amount) sendParam) payable returns (bytes32 sendId)",
]);

/** IntexFactory (outbe precompile): holder-facing settlement + Promis mining. */
export const FACTORY_ABI: Abi = parseAbi([
  "function settle(uint32 seriesId, address intexHolder, uint256 amount)",
  "function minePromis(uint32 seriesId, uint256 amount, uint256 nonce) returns (uint256 promisAmount)",
  "function setAuthorizedSettler(uint32 seriesId, address settler)",
  "event PromisMined(uint32 indexed seriesId, address indexed holder, uint256 amount, uint256 promisAmount)",
]);

/** Minimal ERC20 (BSC payment token; outbe Promis balance). */
export const ERC20_ABI: Abi = parseAbi([
  "function decimals() view returns (uint8)",
  "function symbol() view returns (string)",
  "function balanceOf(address account) view returns (uint256)",
  "function allowance(address owner, address spender) view returns (uint256)",
  "function approve(address spender, uint256 amount) returns (bool)",
]);
