import { type Abi, type Address, getAddress, parseAbi } from "viem";

/**
 * Precompile registry for outbe-chain.
 *
 * Addresses: crates/blockchain/primitives/src/addresses.rs
 * Dispatch:  crates/blockchain/evm/src/precompiles.rs::outbe_dispatch_fn
 * ABIs:      contracts/precompiles/src/I*.sol (human-readable form below).
 *            ITeeRegistry has no .sol — taken from bin/outbe-cli/src/abi.rs.
 *
 * Output parameter names are kept verbatim so `humanize()` (format.ts) can
 * apply per-field formatting (worldwide_day -> date, *Minor -> COEN, etc).
 */

export interface ContractEntry {
  address: Address;
  abi: Abi;
  /** short human description shown in tool output / errors */
  note: string;
}

const A = (hex: string): Address => getAddress(hex);

// --- Shared struct definitions referenced by multiple signatures -------------
const STRUCTS = [
  "struct NodData { uint256 nodId; address owner; uint32 worldwideDay; uint32 leagueId; uint256 floorPriceMinor; uint256 gratisLoadMinor; uint256 costOfGratisMinor; uint256 costAmountMinor; bool isQualified; uint16 issuanceCurrency; uint16 referenceCurrency; uint64 issuedAt; }",
  "struct GemData { uint256 gemId; address owner; uint8 gemType; uint8 state; uint256 gemLoad; uint256 entryPrice; uint256 costAmount; uint256 floorPrice; uint16 issuanceCurrency; uint16 referenceCurrency; uint64 issuedAt; }",
  "struct Position { uint256 positionId; address asset; address bundleAccount; uint256 totalAnadosisAmount; uint256 outstandingAnadosisAmount; uint256 totalGratisAmount; uint256 outstandingGratisAmount; uint32 nextAnadosisNumber; uint64 createdAt; uint256 credisPrincipal; uint256 refinancingRate; uint16 issuanceCurrency; }",
  "struct Anadosis { uint32 anadosisNumber; uint64 dueDate; uint64 paidAt; uint256 anadosisAmount; uint256 gratisAmount; }",
  "struct PledgeTicket { uint256 commitment; uint256 amount; int64 createdAtBlock; }",
  "struct ExchangeRateTuple { string base; string quote; uint256 exchangeRate; uint256 volume; }",
];

export const CONTRACTS: Record<string, ContractEntry> = {
  tribute: {
    address: A("0x0000000000000000000000000000000000001101"),
    note: "Tribute NFT",
    abi: parseAbi([
      "function name() view returns (string)",
      "function symbol() view returns (string)",
      "function totalSupply() view returns (uint256)",
      "function balanceOf(address owner) view returns (uint256)",
      "function ownerOf(uint256 tokenId) view returns (address)",
      "function tokenURI(uint256 tokenId) view returns (string)",
      "function getDayTotals(uint32 worldwideDay) view returns (uint32 tributeCount, uint256 tributeNominalAmountMinor, bool isSealed)",
      "function getTributesByOwner(address owner) view returns (uint256[] tokenIds)",
      "function getTributesByDay(uint32 worldwideDay) view returns (uint256[] tokenIds)",
    ]),
  },

  tributefactory: {
    address: A("0x0000000000000000000000000000000000001100"),
    note: "TributeFactory (offerTribute, enclave decrypt)",
    abi: parseAbi([
      "function offerTribute(bytes cipherText, bytes nonce, uint256 ephemeralPubkey, uint16 referenceCurrency, bytes zkProof, bytes zkVerificationKey, bytes zkPublicKey, bytes zkMerkleRoot) returns (uint256 tributeId)",
    ]),
  },

  nod: {
    address: A("0x0000000000000000000000000000000000001006"),
    note: "Nod NFT",
    abi: parseAbi([
      ...STRUCTS,
      "function name() view returns (string)",
      "function symbol() view returns (string)",
      "function totalSupply() view returns (uint256)",
      "function balanceOf(address owner) view returns (uint256)",
      "function ownerOf(uint256 nodId) view returns (address)",
      "function tokenURI(uint256 nodId) view returns (string)",
      "function nodData(uint256 nodId) view returns (NodData)",
      "function tokens(address owner) view returns (uint256[])",
      "function tokenByIndex(uint256 index) view returns (uint256)",
      "function tokenOfOwnerByIndex(address owner, uint256 index) view returns (uint256)",
    ]),
  },

  gratis: {
    address: A("0x0000000000000000000000000000000000001003"),
    note: "Gratis ERC-20",
    abi: parseAbi([
      "function name() view returns (string)",
      "function symbol() view returns (string)",
      "function decimals() view returns (uint8)",
      "function totalSupply() view returns (uint256)",
      "function pledgedTotalSupply() view returns (uint256)",
      "function balanceOf(address account) view returns (uint256 balanceMinor)",
      "function pledgedOf(address account) view returns (uint256 pledgedMinor)",
      "function allowance(address owner, address spender) view returns (uint256)",
    ]),
  },

  promis: {
    address: A("0x0000000000000000000000000000000000001337"),
    note: "Promis token",
    abi: parseAbi([
      "function name() view returns (string)",
      "function symbol() view returns (string)",
      "function decimals() view returns (uint8)",
      "function totalSupply() view returns (uint256)",
      "function balanceOf(address account) view returns (uint256 balanceMinor)",
    ]),
  },

  promislimit: {
    address: A("0x000000000000000000000000000000000000100F"),
    note: "Promis limit",
    abi: parseAbi(["function totalUnallocated() view returns (uint256)"]),
  },

  gem: {
    address: A("0x0000000000000000000000000000000000001013"),
    note: "Gem NFT",
    abi: parseAbi([
      ...STRUCTS,
      "function name() view returns (string)",
      "function symbol() view returns (string)",
      "function totalSupply() view returns (uint256)",
      "function balanceOf(address owner) view returns (uint256)",
      "function ownerOf(uint256 gemId) view returns (address)",
      "function tokenURI(uint256 gemId) view returns (string)",
      "function tokenOfOwnerByIndex(address owner, uint256 index) view returns (uint256)",
      "function getGemStatus(uint256 gemId) view returns (GemData)",
    ]),
  },

  gemfactory: {
    address: A("0x0000000000000000000000000000000000002013"),
    note: "Gem factory",
    abi: parseAbi([
      "function getStatistics() view returns (uint256 totalGemsIssued, uint256 totalIntexParked)",
    ]),
  },

  credis: {
    address: A("0x000000000000000000000000000000000000100A"),
    note: "Credis positions",
    abi: parseAbi([
      ...STRUCTS,
      "function getPosition(uint256 positionId) view returns (Position)",
      "function getPositionsByAddress(address bundleAccount) view returns (Position[])",
      "function getAllPositions() view returns (Position[])",
      "function hasOverdueAnadosis(address bundleAccount) view returns (bool)",
      "function getNextAnadosis(uint256 positionId) view returns (Anadosis)",
      "function getPositionAnadosis(uint256 positionId) view returns (Anadosis[])",
      "function credisOf(address bundleAccount) view returns (uint256)",
      "function outstandingAnadosisOf(address bundleAccount) view returns (uint256)",
    ]),
  },

  gratisfactory: {
    address: A("0x0000000000000000000000000000000000002003"),
    note: "Gratis factory (pledge tickets)",
    abi: parseAbi([
      ...STRUCTS,
      "function getPledgeTicket(uint256 commitment) view returns (PledgeTicket)",
      "function getPledgeTicketByAddress(address account) view returns (PledgeTicket[])",
      "function getAllPledgeTickets() view returns (PledgeTicket[])",
    ]),
  },

  agentreward: {
    address: A("0x000000000000000000000000000000000000100B"),
    note: "Agent reward",
    abi: parseAbi([
      "function getClaimableBalance(address account) view returns (uint256 balanceMinor)",
      "function claimReward(uint256 amount) returns (uint256)",
    ]),
  },

  fidelity: {
    address: A("0x000000000000000000000000000000000000100C"),
    note: "Fidelity RCFI",
    abi: parseAbi([
      "function getRcfi(address account) view returns (uint64)",
    ]),
  },

  metadosis: {
    address: A("0x000000000000000000000000000000000000100E"),
    note: "Metadosis (WorldwideDay lifecycle)",
    abi: parseAbi([
      "function getWorldwideDay(uint32 wwd) view returns (uint8 status, uint8 dayType, uint64 formingStart, uint64 formingEnd, uint64 lookbackEnd, uint64 offeringEnd, uint64 scheduledProcessTime, uint256 previousVwap, uint256 currentVwap)",
      "function getDayMetadosisLimit(uint32 date) view returns (uint256 amount, bool isUsed)",
      "function getActiveWorldwideDays() view returns (uint32[] wwds)",
      "function getWorldwideDaysByStatus(uint8 status) view returns (uint32[] wwds)",
      "function getBootstrapEndTime() view returns (uint64 endTime)",
    ]),
  },

  oracle: {
    address: A("0x000000000000000000000000000000000000EE05"),
    note: "Oracle (rates, VWAP, pairs)",
    abi: parseAbi([
      ...STRUCTS,
      "function getExchangeRate(string base, string quote) view returns (uint256 rate, uint64 lastBlock, uint64 lastTimestamp)",
      "function getVwap(string base, string quote, uint64 lookbackSeconds) view returns (uint256 vwap)",
      "function getDayVwap(string base, string quote) view returns (uint256 vwap)",
      "function getTwap(string base, string quote, uint64 lookbackSeconds) view returns (uint256 twap)",
      "function getNominalPrice(string base, string quote, uint64 timestamp) view returns (uint256 price)",
      "function getNominalPriceComponents(string base, string quote, uint64 timestamp) view returns (uint256 nominalPrice, uint256 vwap, uint256 maxScurve, string source)",
      "function getParams() view returns (uint64 votePeriod, uint256 rewardBand, uint64 slashWindow, uint256 minValidPerWindow, uint256 slashFraction, uint64 lookbackDuration, bool enabled)",
      "function getPairCount() view returns (uint32 count)",
      "function getPairs() view returns (uint32[] pairIds, string[] bases, string[] quotes, bool[] isActive)",
      "function getVoteTargets() view returns (uint32[] pairIds)",
      "function isVoteTarget(string base, string quote) view returns (bool)",
      "function getReferenceCurrencies() view returns (uint16[] isoCodes)",
      "function getRefinancingRate(uint16 isoCode) view returns (uint256 rate)",
      "function getSettlementCount() view returns (uint32 count)",
      "function getFeederDelegation(address validator) view returns (address feeder)",
      "function getVotePenaltyCounter(address validator) view returns (uint64 success, uint64 abstain, uint64 miss)",
      "function getSlashWindowProgress(address validator) view returns (uint64 success, uint64 abstain, uint64 miss, uint64 slashWindow)",
      "function getAggregateVote(address validator) view returns (bool exists, uint32[] pairIds, uint256[] rates, uint256[] volumes)",
      // curated writes:
      "function submitVote(ExchangeRateTuple[] tuples)",
      "function delegateFeederConsent(address feeder)",
    ]),
  },

  staking: {
    address: A("0x000000000000000000000000000000000000EE02"),
    note: "Staking",
    abi: parseAbi([
      "function getStake(address validator) view returns (uint256 stakeMinor)",
      "function getTotalStaked() view returns (uint256 totalStakedMinor)",
      // curated writes:
      "function stake(address validatorAddress, uint256 amount)",
      "function unstake(uint256 amount)",
      "function claimUnbonded()",
    ]),
  },

  rewards: {
    address: A("0x000000000000000000000000000000000000EE03"),
    note: "Validator rewards",
    abi: parseAbi([
      "function pendingRewards(address validator) view returns (uint256 pendingMinor)",
      // curated write:
      "function claimRewards() returns (uint256)",
    ]),
  },

  validatorset: {
    address: A("0x000000000000000000000000000000000000EE00"),
    note: "Validator set / epoch",
    abi: parseAbi([
      "function getValidators() view returns (address[])",
      "function getActiveValidators() view returns (address[])",
      "function getActiveConsensusSet() view returns (address[])",
      "function validatorCount() view returns (uint32)",
      "function activeValidatorCount() view returns (uint32)",
      "function activeConsensusCount() view returns (uint32)",
      "function isValidator(address addr) view returns (bool)",
      "function isConsensusParticipant(address addr) view returns (bool)",
      "function getEpochNumber() view returns (uint256)",
      "function getEpochStartTimestamp() view returns (uint64)",
      "function getEpochStartBlock() view returns (uint64)",
      "function validatorByAddress(address addr) view returns (address validatorAddress, bytes consensusPubkey, uint256 stakeMinor, uint8 status, uint64 slashCount, uint64 missedBlocks, uint64 missedVotes, uint64 blocksProposed, uint64 joinedAtHeight, uint64 deactivatedAtHeight, uint64 unbondingEnd, bool hasBLSShare)",
    ]),
  },

  slashindicator: {
    address: A("0x000000000000000000000000000000000000EE01"),
    note: "Slash indicator",
    abi: parseAbi([
      "function getProposerMissCount(address validator) view returns (uint64)",
      "function getVoterMissCount(address validator) view returns (uint64)",
      "function getFelonyCount(address validator) view returns (uint64)",
    ]),
  },

  zerofee: {
    address: A("0x000000000000000000000000000000000000EE09"),
    note: "ZeroFee paymaster",
    abi: parseAbi([
      "function getCounter(address signer) view returns (uint32 day, uint32 count)",
    ]),
  },

  teeregistry: {
    address: A("0x000000000000000000000000000000000000EE0A"),
    note: "TEE registry (offer key)",
    abi: parseAbi([
      "function isBootstrapped() view returns (bool)",
      "function tributeOfferPublicKey() view returns (uint256)",
      "function registeredCount() view returns (uint256)",
    ]),
  },
};

/** Resolve a contract by registry name or raw 0x address. */
export function resolveContract(nameOrAddr: string): ContractEntry {
  const key = nameOrAddr.toLowerCase();
  if (CONTRACTS[key]) return CONTRACTS[key];
  if (/^0x[0-9a-fA-F]{40}$/.test(nameOrAddr)) {
    const addr = getAddress(nameOrAddr);
    for (const entry of Object.values(CONTRACTS)) {
      if (entry.address === addr) return entry;
    }
    throw new Error(
      `address ${addr} is not a known precompile; use a contract name (${Object.keys(
        CONTRACTS,
      ).join(", ")})`,
    );
  }
  throw new Error(
    `unknown contract "${nameOrAddr}"; known: ${Object.keys(CONTRACTS).join(", ")}`,
  );
}

// --- Enum / code maps (crates/core/metadosis/src/schema.rs) ------------------
export const WWD_STATUS = [
  "FORMING",
  "LOOKBACK_DELAY",
  "OFFERING",
  "WAITING",
  "READY",
  "IN_PROGRESS",
  "COMPLETED",
  "FAILED",
] as const;

export const DAY_TYPE = ["UNKNOWN", "GREEN", "RED"] as const;

// Gem lifecycle state (crates/core/gem/src/schema.rs::GemState).
export const GEM_STATE = ["Issued", "Qualified", "Settled"] as const;

// ISO 4217 numeric → symbol. Chain currently accepts 840 (USD) only; the rest
// are convenience labels for display.
export const ISO_4217: Record<number, string> = {
  840: "USD",
  978: "EUR",
  826: "GBP",
  392: "JPY",
  756: "CHF",
  156: "CNY",
};

export function statusName(v: number): string {
  return WWD_STATUS[v] ?? `UNKNOWN(${v})`;
}
export function dayTypeName(v: number): string {
  return DAY_TYPE[v] ?? `UNKNOWN(${v})`;
}
export function gemStateName(v: number): string {
  return GEM_STATE[v] ?? `UNKNOWN(${v})`;
}
export function currencyLabel(code: number): { code: number; symbol: string } {
  return { code, symbol: ISO_4217[code] ?? `#${code}` };
}
