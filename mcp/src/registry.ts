import { type Abi, type Address, getAddress, parseAbi } from "viem";

/**
 * Precompile registry for outbe-chain.
 *
 * Addresses: crates/blockchain/primitives/src/addresses.rs
 * Routing:   crates/blockchain/evm/src/precompile_routes.rs (EXACT_ROUTES)
 * ABIs:      contracts/precompiles/src/I*.sol, in viem human-readable form.
 *            ITeeRegistry has no .sol — its signatures come from the
 *            `#[contract_public]` markers in the Rust precompile.
 *
 * This mirrors the FULL inbound ABI of every routed precompile: reads, writes
 * and custom errors. Each entry matches exactly the `I<Name>Calls` enum that
 * the precompile's `dispatch` decodes, so an unknown selector here is an
 * unknown selector on chain. Declaring a write does not expose it as a tool —
 * the signing surface stays the explicit allowlist in src/tools/sign.ts.
 *
 * REWARDS_ADDRESS (0xEE03) has deliberately no entry: its dispatch exposes no
 * callable methods and always reverts (crates/system/rewards/src/precompile.rs).
 *
 * Output parameter names are kept verbatim so `humanize()` (format.ts) can
 * apply per-field formatting (worldwide_day -> date, *Minor -> COEN, etc); a
 * few are renamed off the .sol where a tool already depends on the name, each
 * marked at its use site.
 */

export interface ContractEntry {
  address: Address;
  abi: Abi;
  /** short human description shown in tool output / errors */
  note: string;
}

const A = (hex: string): Address => getAddress(hex);

export const CONTRACTS: Record<string, ContractEntry> = {
  gratis: {
    address: A("0x0000000000000000000000000000000000001003"),
    note:
      "Gratis — confidential (TEE-encrypted) balances; balanceOf/pledgedOf return the account's ciphertext blob (decrypt off-chain with the account's view key from outbe_deriveGratisKeys).",
    abi: parseAbi([
      "function allowance(address owner, address spender) view returns (uint256)",
      "function approve(address spender, uint256 amount) returns (bool)",
      "function balanceOf(address account) view returns (bytes balanceCiphertext)",
      "function decimals() view returns (uint8)",
      "function name() view returns (string)",
      "function opNonceOf(address account) view returns (uint64)",
      "function pledgedOf(address account) view returns (bytes pledgedCiphertext)",
      "function pledgedTotalSupply() view returns (uint256)",
      "function supportsInterface(bytes4 interfaceId) view returns (bool)",
      "function symbol() view returns (string)",
      "function totalSupply() view returns (uint256)",
      "function transfer(address to, uint256 amount) returns (bool)",
      "function transferFrom(address from, address to, uint256 amount) returns (bool)",
    ]),
  },

  nod: {
    address: A("0x0000000000000000000000000000000000001006"),
    note:
      "Nod NFT (36-byte entity ids)",
    abi: parseAbi([
      "struct CertifiedGenerationData { bool exists; uint32 worldwideDay; uint64 generation; bytes32 nodRoot; bytes32 bucketRoot; bytes32 outputManifestRoot; uint32 tributeCount; uint32 nodCount; uint32 bucketCount; uint256 nodAmountTotal; uint256 nodGratisConsumed; uint64 issuedAt; }",
      "struct NodData { bytes nodId; address owner; uint32 worldwideDay; uint16 leagueId; uint256 floorPriceMinor; uint256 gratisLoadMinor; uint256 costOfGratisMinor; uint256 costAmountMinor; bool isQualified; uint16 issuanceCurrency; uint16 referenceCurrency; uint64 issuedAt; }",
      "function balanceOf(address owner) view returns (uint256 balance)",
      "function certifiedGeneration(uint32 worldwideDay) view returns (CertifiedGenerationData)",
      "function name() view returns (string)",
      "function nodData(bytes nodId) view returns (NodData)",
      "function ownerOf(bytes nodId) view returns (address)",
      "function supportsInterface(bytes4 interfaceId) view returns (bool)",
      "function symbol() view returns (string)",
      "function tokenByIndex(uint256 index) view returns (bytes)",
      "function tokenOfOwnerByIndex(address owner, uint256 index) view returns (bytes)",
      "function tokenURI(bytes nodId) view returns (string)",
      "function totalSupply() view returns (uint256)",
    ]),
  },

  nodfactory: {
    address: A("0x0000000000000000000000000000000000001007"),
    note:
      "Nod factory",
    abi: parseAbi([
      "function mineGratis(bytes nodId, uint256 nonce, address asset, bytes32 mac, uint64 opNonce) returns (uint256)",
    ]),
  },

  credisfactory: {
    address: A("0x0000000000000000000000000000000000001009"),
    note:
      "Credis factory",
    abi: parseAbi([
      "function anadosis(uint256 positionId)",
      "function requestCredis(address asset, address bundleAccount, bytes32 pledgeHandle, bytes32 spendAuth) returns (uint256 positionId, uint256 amountStables)",
      "function supportsInterface(bytes4 interfaceId) view returns (bool)",
    ]),
  },

  credis: {
    address: A("0x000000000000000000000000000000000000100a"),
    note:
      "Credis positions",
    abi: parseAbi([
      "struct Position { uint256 positionId; address asset; address bundleAccount; uint256 totalAnadosisAmount; uint256 outstandingAnadosisAmount; uint256 totalGratisAmount; uint256 outstandingGratisAmount; uint32 nextAnadosisNumber; uint64 createdAt; uint256 credisPrincipal; uint256 refinancingRate; uint16 issuanceCurrency; bytes eoaCiphertext; }",
      "struct Anadosis { uint32 anadosisNumber; uint64 dueDate; uint64 paidAt; uint256 anadosisAmount; uint256 gratisAmount; }",
      "function credisOf(address bundleAccount) view returns (uint256)",
      "function getAllPositions() view returns (Position[])",
      "function getNextAnadosis(uint256 positionId) view returns (Anadosis)",
      "function getPosition(uint256 positionId) view returns (Position)",
      "function getPositionAnadosis(uint256 positionId) view returns (Anadosis[])",
      "function getPositionsByAddress(address bundleAccount) view returns (Position[])",
      "function hasOverdueAnadosis(address bundleAccount) view returns (bool)",
      "function outstandingAnadosisOf(address bundleAccount) view returns (uint256)",
      "function supportsInterface(bytes4 interfaceId) view returns (bool)",
    ]),
  },

  agentreward: {
    address: A("0x000000000000000000000000000000000000100b"),
    note:
      "Agent reward",
    abi: parseAbi([
      "function claimReward(uint256 amount) returns (uint256)",
      "function getClaimableBalance(address account) view returns (uint256 balanceMinor)",
    ]),
  },

  fidelity: {
    address: A("0x000000000000000000000000000000000000100c"),
    note:
      "Fidelity RCFI (per-account index is owner-signature-gated; only the public scalars are callable without a signature)",
    abi: parseAbi([
      "function decimals() view returns (uint8)",
      "function getFidelityIndex(address account, uint64 expiry, bytes signature) view returns (uint256)",
      "function getFidelityIndexAt(address account, uint64 timestamp, uint64 expiry, bytes signature) view returns (uint256)",
      "function maxFidelityIndexAt(uint64 timestamp) view returns (uint256)",
      "function maxLeague() view returns (uint16)",
      "function minLeague() view returns (uint16)",
    ]),
  },

  metadosis: {
    address: A("0x000000000000000000000000000000000000100e"),
    note:
      "Metadosis (WorldwideDay lifecycle + OCOMP/Lysis terminal surface)",
    abi: parseAbi([
      "function getActiveLysisGeneration(uint32 wwd) view returns (bytes activeGenerationV1)",
      "function getActiveWorldwideDays() view returns (uint32[] wwds)",
      "function getBootstrapEndTime() view returns (uint64 endTime)",
      "function getCapacityForfeitureReceipt(uint32 wwd) view returns (uint8 outcome, uint32 maxRetainedWorldwideDays, uint32 retainedCountBefore, uint256 valueRouted, uint256 carryOverBefore, uint256 carryOverAfter, bytes32 sealedCollectionRoot, uint32 forfeitedTributeCount, uint256 forfeitedTributeNominal, uint64 sourceGeneration, uint64 retiredGeneration, uint8 retirementOutcome, uint64 blockNumber)",
      "function getLysisTerminalReceipt(bytes32 intentId) view returns (bytes aggregateActivationReceiptV1)",
      "function getOffchainJob(bytes32 intentId) view returns (bytes ocompJobRecordV1)",
      "function getOffchainVoteAccountability(bytes32 jobId) view returns (bytes ocompVoteAccountabilityV1)",
      "function getWorldwideDay(uint32 wwd) view returns (uint8 status, uint8 dayType, uint64 formingStart, uint64 formingEnd, uint64 lookbackEnd, uint64 offeringEnd, uint64 scheduledProcessTime, uint256 previousVwap, uint256 currentVwap)",
      "function getWorldwideDayTerminalReceipt(uint32 wwd) view returns (uint8 outcome, uint256 valueRouted, uint256 carryOverBefore, uint256 carryOverAfter, uint8 retirementOutcome, uint64 blockNumber)",
      "function getWorldwideDaysByStatus(uint8 status) view returns (uint32[] wwds)",
      "function submitLysisResult(bytes resultVoteV1)",
      // custom errors — without them a revert decodes to a bare 4-byte selector
      "error OcompActivationRejected(uint16 code)",
      "error OcompResultVoteRejected(uint16 code)",
    ]),
  },

  promislimit: {
    address: A("0x000000000000000000000000000000000000100f"),
    note:
      "Promis limit",
    abi: parseAbi([
      "function totalUnallocated() view returns (uint256)",
    ]),
  },

  gem: {
    address: A("0x0000000000000000000000000000000000001013"),
    note:
      "Gem NFT",
    abi: parseAbi([
      "struct GemData { uint256 gemId; address owner; uint8 gemType; uint8 state; uint256 gemLoad; uint256 entryPrice; uint256 costAmount; uint256 floorPrice; uint16 issuanceCurrency; uint16 referenceCurrency; uint64 issuedAt; }",
      "function approve(address to, uint256 gemId)",
      "function balanceOf(address owner) view returns (uint256 balance)",
      "function getApproved(uint256 gemId) view returns (address)",
      "function getGemStatus(uint256 gemId) view returns (GemData)",
      "function isApprovedForAll(address owner, address operator) view returns (bool)",
      "function name() view returns (string)",
      "function ownerOf(uint256 gemId) view returns (address)",
      "function safeTransferFrom(address from, address to, uint256 gemId)",
      "function setApprovalForAll(address operator, bool approved)",
      "function symbol() view returns (string)",
      "function tokenOfOwnerByIndex(address owner, uint256 index) view returns (uint256)",
      "function tokenURI(uint256 gemId) view returns (string)",
      "function totalSupply() view returns (uint256)",
      "function transferFrom(address from, address to, uint256 gemId)",
    ]),
  },

  intex: {
    address: A("0x0000000000000000000000000000000000001014"),
    note:
      "Intex series ledger (on-chain precompile side; the cross-chain auction/escrow/NFT ABIs live in src/intex/registry.ts)",
    abi: parseAbi([
      "struct SeriesData { uint32 seriesId; uint256 promisLoadMinor; uint256 entryPriceMinor; uint256 floorPriceMinor; uint32 issuedIntexCount; uint16 callWindowDays; uint16 callThresholdDays; uint256 callPriceMinor; uint8 state; uint32 issuedAt; uint32 calledAt; uint32 intexCallPeriod; uint16 issuanceCurrency; uint16 referenceCurrency; }",
      "function seriesAt(uint64 index) view returns (uint32)",
      "function seriesData(uint32 seriesId) view returns (SeriesData)",
      "function seriesExists(uint32 seriesId) view returns (bool)",
      "function totalSeries() view returns (uint64)",
    ]),
  },

  intexfactory: {
    address: A("0x0000000000000000000000000000000000001015"),
    note:
      "Intex factory / settlement",
    abi: parseAbi([
      "function distribute(uint32 worldwideDay, uint32 srcChainId) payable",
      "function minePromis(uint32 seriesId, uint256 amount, uint256 nonce, bytes32 mac, uint64 opNonce) returns (uint256 promisAmount)",
      "function setAuthorizedSettler(uint32 seriesId, address settler)",
      "function settle(uint32 seriesId, address intexHolder, uint256 amount)",
    ]),
  },

  desis: {
    address: A("0x0000000000000000000000000000000000001016"),
    note:
      "Desis",
    abi: parseAbi([
      "function getAuctionStage(uint32 worldwideDay) view returns (uint8)",
      "function getBidsCount(uint32 worldwideDay) view returns (uint256)",
      "function getChainBidsCount(uint32 worldwideDay, uint32 srcChainId) view returns (uint256)",
      "function isChainDone(uint32 worldwideDay, uint32 srcChainId) view returns (bool)",
      "function processBidsBatch(uint32 worldwideDay, uint32 srcChainId, uint32 relayGeneration, uint16 batchIndex, uint16 totalBatches, address[] bidderAddresses, uint16[] intexQuantities, uint32[] intexBidRates, uint32[] timestamps)",
      "function processBidsDone(uint32 worldwideDay, uint32 srcChainId, uint32 relayGeneration, uint16 totalBatches, uint32 totalBids)",
      "function supportsInterface(bytes4 interfaceId) view returns (bool)",
    ]),
  },

  vaultrouter: {
    address: A("0x0000000000000000000000000000000000001017"),
    note:
      "Vault router (+ crosschain extension; both Calls enums are dispatched at this address)",
    abi: parseAbi([
      "function addLiquiditySource(address sourceAddress, uint8 sourceType)",
      "function addLiquidityTarget(address targetAddress, uint8 targetType)",
      "function addVault(address vault)",
      "function assetAt(uint256 index) view returns (address asset)",
      "function assetVaultAt(address asset, uint256 index) view returns (address vault)",
      "function assetVaultsCount(address asset) view returns (uint256)",
      "function assetsCount() view returns (uint256)",
      "function crosschainAsset() view returns (address)",
      "function crosschainBridge() view returns (address)",
      "function crosschainDeposit(uint256 assetsAmount, uint256 destinationGasLimit, uint256 acknowledgementGasLimit) payable returns (bytes32 operationId, bytes32 sendId)",
      "function crosschainDestinationChainId() view returns (uint256)",
      "function crosschainOperation(bytes32 operationId) view returns (address user, uint256 amount, uint8 kind, uint8 status)",
      "function crosschainOperationNonce() view returns (uint256)",
      "function crosschainShares(address user) view returns (uint256)",
      "function crosschainTokenBridge() view returns (address)",
      "function crosschainWithdraw(uint256 sharesAmount, uint256 requestGasLimit, uint256 returnGasLimit) payable returns (bytes32 operationId, bytes32 sendId)",
      "function deposit(address asset, uint256 assetsAmount) returns (uint256 sharesAmount)",
      "function liquiditySourceAt(uint256 index) view returns (address sourceAddress, uint8 sourceType)",
      "function liquiditySourcesCount() view returns (uint256)",
      "function liquidityTargetAt(uint256 index) view returns (address targetAddress, uint8 targetType)",
      "function liquidityTargetsCount() view returns (uint256)",
      "function onCrosschainTokensReceived(uint32 sourceDomain, bytes from, uint256 amount, bytes extraData) returns (bytes4)",
      "function owner() view returns (address)",
      "function pendingCrosschainOperations() view returns (uint256)",
      "function quoteCrosschainDeposit(uint256 assetsAmount, uint256 destinationGasLimit, uint256 acknowledgementGasLimit) view returns (uint256 nativeFee, bytes32 operationId)",
      "function quoteCrosschainWithdraw(uint256 sharesAmount, uint256 requestGasLimit, uint256 returnGasLimit) view returns (uint256 nativeFee, bytes32 operationId)",
      "function receiveMessage(bytes32 receiveId, bytes sender, bytes payload) payable returns (bytes4)",
      "function referenceCurrencyVaultAt(uint16 isoCode, uint256 index) view returns (address vault)",
      "function referenceCurrencyVaultsCount(uint16 isoCode) view returns (uint256)",
      "function remoteVaultRouter(uint256 chainId) view returns (address)",
      "function removeLiquiditySource(address sourceAddress)",
      "function removeLiquidityTarget(address targetAddress)",
      "function removeVault(address vault)",
      "function setCrosschainAsset(address asset, address tokenBridge, uint256 destinationChainId)",
      "function setCrosschainBridge(address bridge)",
      "function setRemoteVaultRouter(uint256 chainId, address router)",
      "function sharesBalance(address vault) view returns (uint256)",
      "function totalCrosschainShares() view returns (uint256)",
      "function vaultReferenceCurrency(address vault) view returns (uint16 isoCode)",
      "function withdraw(address asset, uint256 amount, address receiver) returns (uint256 burnedShares)",
      // custom errors — without them a revert decodes to a bare 4-byte selector
      "error CrosschainAssetNotConfigured()",
      "error CrosschainBridgeNotConfigured()",
      "error CrosschainFeeMismatch(uint256 provided, uint256 required)",
      "error CrosschainOperationAlreadyCompleted(bytes32 operationId)",
      "error CrosschainOperationNotFound(bytes32 operationId)",
      "error CrosschainOperationsPending(uint256 count)",
      "error CrosschainTokenBridgeNotConfigured()",
      "error InsufficientCrosschainShares(uint256 availableShares, uint256 requiredShares)",
      "error InsufficientSharesForWithdraw(uint256 availableShares, uint256 requiredShares)",
      "error InvalidCrosschainCallback()",
      "error InvalidCrosschainSender()",
      "error InvalidDestinationChain()",
      "error InvalidLiquiditySource()",
      "error InvalidLiquidityTarget()",
      "error InvalidReferenceCurrency()",
      "error LiquiditySourceNotFound()",
      "error LiquidityTargetNotFound()",
      "error RemoteVaultRouterNotConfigured(uint256 chainId)",
      "error ReserveVaultAlreadyAdded()",
      "error ReserveVaultAssetMismatch()",
      "error ReserveVaultNotConfigured()",
      "error ReserveVaultNotFound()",
      "error ReserveVaultOwnerNotRenounced(address currentOwner)",
    ]),
  },

  governance: {
    address: A("0x0000000000000000000000000000000000001018"),
    note:
      "Governance (canon, meta-canon, OIP, GIP)",
    abi: parseAbi([
      "struct Gip { uint256 id; uint8 statusCode; address author; uint64 createdBlock; uint64 updatedBlock; bytes32 textHash; string text; }",
      "struct ProposalMeta { uint256 id; uint8 statusCode; address author; uint64 createdBlock; uint64 updatedBlock; bytes32 textHash; }",
      "struct Oip { uint256 id; uint8 statusCode; address author; uint64 createdBlock; uint64 updatedBlock; bytes32 textHash; string text; }",
      "function getCanon() view returns (string text, uint64 version, bytes32 hash)",
      "function getCanonRevisionHash(uint64 version) view returns (bytes32)",
      "function getGip(uint256 id) view returns (Gip)",
      "function getGipDiff(uint256 id, uint8 base) view returns (string)",
      "function getGipsByAuthor(address author, uint256 offset, uint256 limit) view returns (ProposalMeta[])",
      "function getGipsByStatus(uint8 status, uint256 offset, uint256 limit) view returns (ProposalMeta[])",
      "function getMetaCanon() view returns (string text, uint64 version, bytes32 hash)",
      "function getMetaCanonRevisionHash(uint64 version) view returns (bytes32)",
      "function getOip(uint256 id) view returns (Oip)",
      "function getOipDiff(uint256 id, uint8 base) view returns (string)",
      "function getOipsByAuthor(address author, uint256 offset, uint256 limit) view returns (ProposalMeta[])",
      "function getOipsByStatus(uint8 status, uint256 offset, uint256 limit) view returns (ProposalMeta[])",
      "function gipCount() view returns (uint64)",
      "function gipCountByAuthor(address author) view returns (uint256)",
      "function gipCountByStatus(uint8 status) view returns (uint256)",
      "function isAuthority(address who) view returns (bool)",
      "function oipCount() view returns (uint64)",
      "function oipCountByAuthor(address author) view returns (uint256)",
      "function oipCountByStatus(uint8 status) view returns (uint256)",
      "function setGipStatus(uint256 id, uint8 newStatus)",
      "function setOipStatus(uint256 id, uint8 newStatus)",
      "function submitGip(string text) returns (uint256 id)",
      "function submitOip(string text) returns (uint256 id)",
      "function updateCanon(string text) returns (uint64 newVersion)",
      "function updateGipText(uint256 id, string text)",
      "function updateMetaCanon(string text) returns (uint64 newVersion)",
      "function updateOipText(uint256 id, string text)",
    ]),
  },

  tributefactory: {
    address: A("0x0000000000000000000000000000000000001100"),
    note:
      "TributeFactory (offerTribute, enclave decrypt)",
    abi: parseAbi([
      "function offerTribute(bytes cipherText, bytes nonce, uint256 ephemeralPubkey, uint16 referenceCurrency, bool excludeFromIntexIssuance, bytes zkProof, bytes zkVerificationKey, bytes zkPublicKey, bytes zkMerkleRoot, bytes signature) returns (bytes tributeId)",
    ]),
  },

  tribute: {
    address: A("0x0000000000000000000000000000000000001101"),
    note:
      "Tribute NFT (36-byte entity ids)",
    abi: parseAbi([
      "function balanceOf(address owner) view returns (uint256)",
      "function getDayTotals(uint32 worldwideDay) view returns (uint32 tributeCount, uint256 tributeNominalAmountMinor, bool isSealed)",
      "function getTributesByDay(uint32 worldwideDay) view returns (bytes[] tributeIds)",
      "function getTributesByOwner(address owner) view returns (bytes[] tributeIds)",
      "function name() view returns (string)",
      "function ownerOf(bytes tributeId) view returns (address)",
      "function supportsInterface(bytes4 interfaceId) view returns (bool)",
      "function symbol() view returns (string)",
      "function tokenURI(bytes tributeId) view returns (string)",
      "function totalSupply() view returns (uint256)",
    ]),
  },

  promis: {
    address: A("0x0000000000000000000000000000000000001337"),
    note:
      "Promis — confidential (TEE-encrypted) balances; balanceOf returns the account's ciphertext blob (decrypt off-chain with the account's view key from outbe_deriveKeys(Promis, ...)). opNonceOf is the modify-auth replay counter a write's mac/opNonce must bind.",
    abi: parseAbi([
      "function balanceOf(address account) view returns (bytes balanceCiphertext)",
      "function decimals() view returns (uint8)",
      "function name() view returns (string)",
      "function opNonceOf(address account) view returns (uint64)",
      "function supportsInterface(bytes4 interfaceId) view returns (bool)",
      "function symbol() view returns (string)",
      "function totalSupply() view returns (uint256)",
    ]),
  },

  gratisfactory: {
    address: A("0x0000000000000000000000000000000000002003"),
    note:
      "Gratis factory (mine / pledge / unpledge)",
    abi: parseAbi([
      "function mineCoen(uint256 amount, bytes32 mac, uint64 opNonce) returns (uint256)",
      "function mineFromPromis(uint256 amount, bytes32 mac, uint64 opNonce, bytes32 promisMac, uint64 promisOpNonce) returns (uint256)",
      "function pledgeGratis(uint256 amount, bytes32 mac, uint64 opNonce) returns (bytes32 pledgeHandle)",
      "function supportsInterface(bytes4 interfaceId) view returns (bool)",
      "function unpledgeGratis(uint256 amount, bytes32 pledgeHandle, bytes32 mac, uint64 opNonce)",
    ]),
  },

  gemfactory: {
    address: A("0x0000000000000000000000000000000000002013"),
    note:
      "Gem factory",
    abi: parseAbi([
      "function getStatistics() view returns (uint256 totalGemsIssued, uint256 totalIntexParked)",
      "function mineGemPromis(uint256 gemId, uint256 nonce, bytes32 mac, uint64 opNonce) returns (uint256)",
      "function settleGem(uint256 gemId)",
    ]),
  },

  promisfactory: {
    address: A("0x0000000000000000000000000000000000002337"),
    note:
      "Promis factory",
    abi: parseAbi([
      "function mineCoen(uint256 amount, bytes32 mac, uint64 opNonce) returns (uint256)",
      "function supportsInterface(bytes4 interfaceId) view returns (bool)",
    ]),
  },

  validatorset: {
    address: A("0x000000000000000000000000000000000000ee00"),
    note:
      "Validator set / epoch",
    abi: parseAbi([
      "function activateResharedSet(address[] newActiveSet, bytes32 groupPublicKey)",
      "function activeConsensusCount() view returns (uint32)",
      "function activeValidatorCount() view returns (uint32)",
      "function confirmValidatorReady()",
      "function deactivateValidator(address validatorAddress)",
      "function getActiveConsensusSet() view returns (address[])",
      "function getActiveValidators() view returns (address[])",
      "function getDelegate(address validator, uint8 role) view returns (address)",
      "function getEpochNumber() view returns (uint256)",
      "function getEpochStartBlock() view returns (uint64)",
      "function getEpochStartTimestamp() view returns (uint64)",
      "function getP2pAddress(address validatorAddress) view returns (uint8 version, bytes encoded)",
      "function getValidators() view returns (address[])",
      "function hasPendingSetChange() view returns (bool)",
      "function isConsensusParticipant(address addr) view returns (bool)",
      "function isValidator(address addr) view returns (bool)",
      "function registerValidator(address validatorAddress, bytes consensusPubkey, bytes blsSignature)",
      "function resolveValidator(uint8 role, address signer) view returns (address)",
      "function revokeDelegate(uint8 role)",
      "function setDelegate(uint8 role, address delegate)",
      "function setP2pAddress(address validatorAddress, uint8 version, bytes encoded)",
      "function validatorByAddress(address addr) view returns (address validatorAddress, bytes consensusPubkey, uint256 stakeMinor, uint8 status, uint64 slashCount, uint64 missedBlocks, uint64 missedVotes, uint64 blocksProposed, uint64 joinedAtHeight, uint64 deactivatedAtHeight, uint64 unbondingEnd, bool hasBLSShare)",
      "function validatorByIndex(uint64 index) view returns (address validatorAddress, bytes consensusPubkey, uint256 stakeMinor, uint8 status, uint64 slashCount, uint64 missedBlocks, uint64 missedVotes, uint64 blocksProposed, uint64 joinedAtHeight, uint64 deactivatedAtHeight, uint64 unbondingEnd, bool hasBLSShare)",
      "function validatorCount() view returns (uint32)",
    ]),
  },

  slashindicator: {
    address: A("0x000000000000000000000000000000000000ee01"),
    note:
      "Slash indicator (miss counters + byzantine evidence ingress)",
    abi: parseAbi([
      "function getFelonyCount(address validator) view returns (uint64)",
      "function getProposerMissCount(address validator) view returns (uint64)",
      "function getVoterMissCount(address validator) view returns (uint64)",
      "function submitConflictingFinalizeEvidence(bytes block1, bytes block2)",
      "function submitConflictingNotarizeEvidence(bytes block1, bytes block2)",
      "function submitConflictingVoteEvidence(bytes vote1, bytes vote2)",
      "function submitDoubleProposalEvidence(bytes block1, bytes block2)",
      "function submitInvalidSeedPartialEvidence(bytes evidence)",
      "function submitInvalidVrfProofEvidence(bytes evidence)",
      "function submitNullifyFinalizeEvidence(bytes nullifyBlock, bytes finalizeBlock)",
      "function submitSeedPartialEquivocationEvidence(bytes evidence)",
    ]),
  },

  staking: {
    address: A("0x000000000000000000000000000000000000ee02"),
    note:
      "Staking",
    abi: parseAbi([
      "function claimUnbonded()",
      "function getStake(address validator) view returns (uint256 stakeMinor)",
      "function getTotalStaked() view returns (uint256 totalStakedMinor)",
      "function stake(address validatorAddress, uint256 amount) payable",
      "function unjailValidator()",
      "function unstake(uint256 amount)",
    ]),
  },

  oracle: {
    address: A("0x000000000000000000000000000000000000ee05"),
    note:
      "Oracle (rates, VWAP, TWAP, s-curve, pairs)",
    abi: parseAbi([
      "struct ExchangeRateTuple { string base; string quote; uint256 exchangeRate; uint256 volume; }",
      "function activateVoteTarget(string base, string quote)",
      "function deactivateVoteTarget(string base, string quote)",
      "function delegateFeederConsent(address feeder)",
      "function getAggregateVote(address validator) view returns (bool exists, uint32[] pairIds, uint256[] rates, uint256[] volumes)",
      "function getAllPriceSnapshotHistory(uint32 count) view returns (uint64[] snapshotIds, uint64[] timestamps, uint32[] pairIds, uint256[] rates, uint256[] volumes)",
      "function getAllScurveData() view returns (uint32[] pairIds, uint64[] peakDays, uint256[] peakPrices)",
      "function getAllScurveDataForPair(string base, string quote) view returns (uint64[] peakDays, uint256[] peakPrices)",
      "function getDayVwap(string base, string quote) view returns (uint256 vwap)",
      "function getExchangeRate(string base, string quote) view returns (uint256 rate, uint64 lastBlock, uint64 lastTimestamp)",
      "function getExchangeRates() view returns (uint256[] rates, uint64[] blocks, uint64[] timestamps)",
      "function getFeederDelegation(address validator) view returns (address feeder)",
      "function getNominalPrice(string base, string quote, uint64 timestamp) view returns (uint256 price)",
      "function getNominalPriceComponents(string base, string quote, uint64 timestamp) view returns (uint256 nominalPrice, uint256 vwap, uint256 maxScurve, string source)",
      "function getPairCount() view returns (uint32 count)",
      "function getPairs() view returns (uint32[] pairIds, string[] bases, string[] quotes, bool[] isActive)",
      "function getParams() view returns (uint64 votePeriod, uint256 rewardBand, uint64 slashWindow, uint256 minValidPerWindow, uint256 slashFraction, uint64 lookbackDuration, bool enabled)",
      "function getPriceSnapshotHistory(string base, string quote, uint32 count) view returns (uint64[] timestamps, uint256[] rates, uint256[] volumes)",
      "function getReferenceCurrencies() view returns (uint16[] isoCodes)",
      "function getRefinancingRate(uint16 isoCode) view returns (uint256 rate)",
      "function getScurveEntries(string base, string quote) view returns (uint64[] peakDays, uint256[] peakPrices, uint256[] currentValues)",
      "function getScurveValue(string base, string quote, uint64 timestamp) view returns (uint256 value)",
      "function getScurveValues(string base, string quote, uint64 timestamp) view returns (uint64 targetDay, uint64[] peakDays, uint256[] peakPrices, uint256[] values)",
      "function getSettlementCount() view returns (uint32 count)",
      "function getSettlementCurrencies() view returns (uint16[] isoCodes, string[] denoms, bytes32[] denomHashes, bytes32[] pairHashes)",
      "function getSettlementCurrency(uint16 isoCode) view returns (bytes32 denomHash, bytes32 pairHash)",
      "function getSlashWindowProgress(address validator) view returns (uint64 success, uint64 abstain, uint64 miss, uint64 slashWindow)",
      "function getTwap(string base, string quote, uint64 lookbackSeconds) view returns (uint256 twap)",
      "function getTwaps(uint64 lookback) view returns (uint32[] pairIds, uint256[] twaps, uint64[] lookbackSeconds)",
      "function getUtcDayVwap(string base, string quote, uint32 utcDay) view returns (uint256 vwap)",
      "function getVotePenaltyCounter(address validator) view returns (uint64 success, uint64 abstain, uint64 miss)",
      "function getVoteTargets() view returns (uint32[] pairIds)",
      "function getVwap(string base, string quote, uint64 lookbackSeconds) view returns (uint256 vwap)",
      "function getVwapForTimeRange(string base, string quote, uint64 startTime, uint64 endTime) view returns (uint256 vwap)",
      "function getWorldwideDayVwap(uint64 startTime, uint64 endTime) view returns (uint32[] pairIds, uint256[] vwaps, uint64[] lookbackSeconds)",
      "function getWorldwideDayVwapSnapshot(uint32 worldwideDay) view returns (uint64 startTime, uint64 endTime, uint32[] pairIds, uint256[] vwaps, uint64[] lookbackSeconds)",
      "function isVoteTarget(string base, string quote) view returns (bool)",
      "function setExchangeRate(string base, string quote, uint256 rate)",
      "function submitVote(ExchangeRateTuple[] tuples)",
    ]),
  },

  zerofee: {
    address: A("0x000000000000000000000000000000000000ee09"),
    note:
      "ZeroFee paymaster",
    abi: parseAbi([
      "function authorizeSponsorship(address signer) view returns (bool)",
      "function getCounter(address signer) view returns (uint32 day, uint32 count)",
    ]),
  },

  teeregistry: {
    address: A("0x000000000000000000000000000000000000ee0a"),
    note:
      "TEE registry (offer key, enclave keys, policy). No ITeeRegistry.sol — signatures come from the #[contract_public] markers in crates/system/teeregistry/src/precompile.rs. 32-byte keys are returned as uint256 (the same 32 bytes); reinterpret as bytes32.",
    abi: parseAbi([
      "function isBootstrapped() view returns (bool)",
      "function keyEpoch() view returns (uint256)",
      "function keysHash(address validator) view returns (uint256)",
      "function noiseStaticPubkey(address validator) view returns (uint256)",
      "function policyHash() view returns (uint256)",
      "function recipientX25519(address validator) view returns (uint256)",
      "function registerEnclave(uint256 recipientX25519, uint256 attestationPub, uint256 noiseStaticPub, uint256 mrenclave, uint256 mrsigner, uint16 isvSvn) returns (bool)",
      "function registeredCount() view returns (uint256)",
      "function tributeOfferEpoch() view returns (uint256)",
      "function tributeOfferPublicKey() view returns (uint256)",
    ]),
  },

  update: {
    address: A("0x000000000000000000000000000000000000ee0b"),
    note:
      "Update (hard-fork version scheduling / activation)",
    abi: parseAbi([
      "struct ScheduledUpdate { uint256 proposalId; uint32 version; uint64 activationHeight; bytes info; uint8 statusCode; }",
      "function getActiveVersion() view returns (uint32)",
      "function getActiveVersionHeight() view returns (uint64)",
      "function getScheduledUpdate(uint256 proposalId) view returns (ScheduledUpdate)",
      "function isVersionActive(uint32 version) view returns (bool)",
      "function listWaitingForActivation() view returns (uint256[])",
    ]),
  },

  vote: {
    address: A("0x000000000000000000000000000000000000ee0c"),
    note:
      "Vote (module-target proposals + proposal bonds)",
    abi: parseAbi([
      "struct VoteTally { uint64 yes; uint64 no; }",
      "struct ProposalInfo { uint256 proposalId; address proposer; address targetModule; string payload; uint64 createdHeight; uint64 votingDeadlineHeight; uint8 statusCode; VoteTally state; uint256 votersCount; }",
      "function castVote(uint256 proposalId, bool approve)",
      "function createProposal(address targetModule, string payload) payable returns (uint256 proposalId)",
      "function getProposal(uint256 proposalId) view returns (ProposalInfo)",
      "function getProposalBond(uint256 proposalId) view returns (uint256 amountMinor, uint8 settlement)",
      "function getProposalVoters(uint256 proposalId, uint256 index, uint256 count) view returns (address[])",
      "function listProposals(uint256 index, uint256 count) view returns (uint256[])",
      "function listProposalsByStatus(uint8 status, uint256 index, uint256 count) view returns (uint256[])",
      "function unsettledBondLiabilities() view returns (uint256 amountMinor)",
      // custom errors — without them a revert decodes to a bare 4-byte selector
      "error InvalidProposalBond(uint256 expected, uint256 actual)",
      "error ProposalBondAlreadySettled(uint256 proposalId, uint8 settlement)",
      "error ProposalBondLiabilityInvariant(uint256 balance, uint256 liabilities)",
      "error ProposalBondSettlementFailed(uint256 proposalId)",
    ]),
  },

  l2registry: {
    address: A("0x000000000000000000000000000000000000ee0e"),
    note:
      "L2 registry (per-network L1 address, public key, ZK gate)",
    abi: parseAbi([
      "function chainIdByL1Address(address l1Address) view returns (uint64)",
      "function getNetwork(uint64 chainId) view returns (address l1Address, bytes publicKey, bool zkEnabled)",
      "function registerNetwork(uint64 chainId, address l1Address, bytes publicKey)",
      "function removeNetwork(uint64 chainId)",
      "function setZkEnabled(uint64 chainId, bool enabled)",
    ]),
  },

  stablecoinfactory: {
    address: A("0x000000000000000000000000000000000000ee0f"),
    note:
      "Stablecoin Factory — issued tokens live at dynamic 0x53c0… addresses; call those by their 0x address (see STABLECOIN_ADDRESS_PREFIX).",
    abi: parseAbi([
      "function isStablecoin(address token) view returns (bool)",
      "function listTokens(uint256 offset, uint256 limit) view returns (address[])",
      "function predictTokenAddress(address issuer, string ticker) view returns (bytes32 tokenId, address token)",
      "function tokenById(bytes32 tokenId) view returns (address)",
      "function tokenByTicker(string ticker) view returns (address)",
      "function tokenCount() view returns (uint256)",
      "function tokenIdOf(address token) view returns (bytes32)",
      // custom errors — without them a revert decodes to a bare 4-byte selector
      "error InvalidIssuer(address issuer)",
      "error InvalidListLimit(uint256 limit, uint256 maximum)",
      "error InvalidTicker(string ticker)",
      "error NonCanonicalPayload()",
      "error TickerAlreadyRegistered(string ticker, address token)",
      "error TickerReserved(string ticker, uint256 proposalId)",
      "error TokenAddressCollision(address token, bytes32 existingTokenId, bytes32 requestedTokenId)",
      "error TokenIdAlreadyRegistered(bytes32 tokenId, address token)",
      "error TokenIdReserved(bytes32 tokenId, uint256 proposalId)",
      "error UnexpectedValue(uint256 value)",
      "error UnknownPolicy(uint256 policyId)",
      "error UnknownStablecoin(address token)",
    ]),
  },

  stablecoinpolicy: {
    address: A("0x000000000000000000000000000000000000ee10"),
    note:
      "Stablecoin Policy Registry (membership + send / receive / mint gates)",
    abi: parseAbi([
      "function acceptPolicyAdminTransfer(uint256 policyId)",
      "function addMembers(uint256 policyId, address[] accounts)",
      "function beginPolicyAdminTransfer(uint256 policyId, address candidate)",
      "function canMint(uint256 policyId, address account) view returns (bool)",
      "function canReceive(uint256 policyId, address account) view returns (bool)",
      "function canSend(uint256 policyId, address account) view returns (bool)",
      "function cancelPolicyAdminTransfer(uint256 policyId)",
      "function createDirectionalPolicy(address admin, uint256 sendPolicyId, uint256 receivePolicyId, uint256 mintPolicyId) returns (uint256 policyId)",
      "function createPolicy(uint8 policyType, address admin) returns (uint256 policyId)",
      "function isMember(uint256 policyId, address account) view returns (bool)",
      "function listPolicyMembers(uint256 policyId, uint256 offset, uint256 limit) view returns (address[])",
      "function pendingPolicyAdmin(uint256 policyId) view returns (address)",
      "function policyAdmin(uint256 policyId) view returns (address)",
      "function policyExists(uint256 policyId) view returns (bool)",
      "function policyMemberCount(uint256 policyId) view returns (uint256)",
      "function policyType(uint256 policyId) view returns (uint8)",
      "function removeMembers(uint256 policyId, address[] accounts)",
      // custom errors — without them a revert decodes to a bare 4-byte selector
      "error DirectionalMembershipUnsupported(uint256 policyId)",
      "error DuplicateMember(address account)",
      "error EmptyMemberBatch()",
      "error ImmutablePolicy(uint256 policyId)",
      "error InvalidDirectionalChild(uint256 policyId)",
      "error InvalidListLimit(uint256 limit, uint256 maximum)",
      "error InvalidMember(address account)",
      "error InvalidPendingPolicyAdmin(address candidate)",
      "error InvalidPolicyAdmin(address admin)",
      "error InvalidPolicyType(uint8 policyType)",
      "error MemberBatchTooLarge(uint256 length, uint256 maximum)",
      "error MembershipUnchanged(uint256 policyId, address account, bool member)",
      "error NotPendingPolicyAdmin(uint256 policyId, address caller)",
      "error PolicyIdExhausted()",
      "error PolicyMemberEnumerationUnsupported(uint256 policyId, uint8 policyType)",
      "error UnauthorizedPolicyAdmin(uint256 policyId, address caller)",
      "error UnexpectedValue(uint256 value)",
      "error UnknownPolicy(uint256 policyId)",
    ]),
  },
};

/**
 * Factory-issued stablecoin tokens live at *dynamic* addresses inside a
 * genesis-reserved two-byte class, so they get no fixed registry entry:
 * `resolveContract` binds this ABI to any address in the class
 * (`STABLECOIN_ADDRESS_PREFIX`, crates/blockchain/primitives/src/addresses.rs).
 * The chain still rejects an address the Factory never issued.
 */
export const STABLECOIN_ADDRESS_PREFIX = "0x53c0";

export const STABLECOIN_ABI: Abi = parseAbi([
  "function DOMAIN_SEPARATOR() view returns (bytes32)",
  "function acceptAdminTransfer()",
  "function allowance(address owner, address spender) view returns (uint256)",
  "function approve(address spender, uint256 value) returns (bool)",
  "function balanceOf(address account) view returns (uint256)",
  "function beginAdminTransfer(address candidate)",
  "function burn(uint256 amount) returns (bool)",
  "function burnFrom(address from, uint256 amount) returns (bool)",
  "function burnFromWithMemo(address from, uint256 amount, bytes32 memo) returns (bool)",
  "function burnWithMemo(uint256 amount, bytes32 memo) returns (bool)",
  "function canReceive(address account) view returns (bool status)",
  "function canSend(address account) view returns (bool status)",
  "function canTransfer(address from, address to, uint256 value) view returns (bool status)",
  "function cancelAdminTransfer()",
  "function creationProtocolVersion() view returns (uint64)",
  "function currency() view returns (uint16)",
  "function decimals() view returns (uint8)",
  "function forcedTransfer(address from, address to, uint256 value) returns (bool result)",
  "function forcedTransferWithMemo(address from, address to, uint256 amount, bytes32 memo) returns (bool)",
  "function getFrozenTokens(address account) view returns (uint256 amount)",
  "function grantRole(bytes32 role, address account)",
  "function hasRole(bytes32 role, address account) view returns (bool)",
  "function issuer() view returns (address)",
  "function mint(address to, uint256 amount) returns (bool)",
  "function mintWithMemo(address to, uint256 amount, bytes32 memo) returns (bool)",
  "function name() view returns (string)",
  "function nonces(address owner) view returns (uint256)",
  "function pause()",
  "function paused() view returns (bool)",
  "function pendingAdmin() view returns (address)",
  "function permit(address owner, address spender, uint256 value, uint256 deadline, uint8 v, bytes32 r, bytes32 s)",
  "function policyId() view returns (uint256)",
  "function revokeRole(bytes32 role, address account)",
  "function setFrozenTokens(address account, uint256 amount) returns (bool result)",
  "function setPolicyId(uint256 newPolicyId)",
  "function setSupplyCap(uint256 newCap)",
  "function supplyCap() view returns (uint256)",
  "function supportsInterface(bytes4 interfaceId) view returns (bool)",
  "function symbol() view returns (string)",
  "function totalSupply() view returns (uint256)",
  "function transfer(address to, uint256 value) returns (bool)",
  "function transferFrom(address from, address to, uint256 value) returns (bool)",
  "function transferFromWithMemo(address from, address to, uint256 amount, bytes32 memo) returns (bool)",
  "function transferWithMemo(address to, uint256 amount, bytes32 memo) returns (bool)",
  "function unpause()",
  // custom errors — without them a revert decodes to a bare 4-byte selector
  "error ERC7943CannotReceive(address account)",
  "error ERC7943CannotSend(address account)",
  "error ERC7943CannotTransfer(address from, address to, uint256 amount)",
  "error ERC7943InsufficientUnfrozenBalance(address account, uint256 amount, uint256 unfrozen)",
  "error ForcedSelfTransfer()",
  "error FrozenAllowanceIncrease(address owner, uint256 currentAllowance, uint256 requestedAllowance)",
  "error InsufficientAllowance(address owner, address spender, uint256 available, uint256 required)",
  "error InsufficientBalance(address account, uint256 available, uint256 required)",
  "error InvalidAddress(address account)",
  "error InvalidAmount()",
  "error InvalidPendingAdmin(address candidate)",
  "error InvalidPermitSignature()",
  "error MigrationRequired(uint64 storedSchemaVersion, uint64 activeSchemaVersion)",
  "error NotPendingAdmin(address caller)",
  "error PermitExpired(uint256 deadline)",
  "error PolicyDenied(uint256 policyId, address account, uint8 operation)",
  "error SupplyCapBelowSupply(uint256 requestedCap, uint256 totalSupply)",
  "error SupplyCapExceeded(uint256 supplyCap, uint256 requestedSupply)",
  "error TokenNotPaused()",
  "error TokenPaused()",
  "error Unauthorized(bytes32 role, address caller)",
  "error UnexpectedValue(uint256 value)",
  "error UnknownPolicy(uint256 policyId)",
  "error UnsupportedRole(bytes32 role)",
]);

/** Resolve a contract by registry name or raw 0x address. */
export function resolveContract(nameOrAddr: string): ContractEntry {
  const key = nameOrAddr.toLowerCase();
  if (CONTRACTS[key]) return CONTRACTS[key];
  if (/^0x[0-9a-fA-F]{40}$/.test(nameOrAddr)) {
    const addr = getAddress(nameOrAddr);
    for (const entry of Object.values(CONTRACTS)) {
      if (entry.address === addr) return entry;
    }
    if (key.startsWith(STABLECOIN_ADDRESS_PREFIX)) {
      return {
        address: addr,
        abi: STABLECOIN_ABI,
        note: "Factory-issued stablecoin token (dynamic address class)",
      };
    }
    throw new Error(
      `address ${addr} is not a known precompile; use a contract name (${Object.keys(
        CONTRACTS,
      ).join(", ")}) or a ${STABLECOIN_ADDRESS_PREFIX}… stablecoin token address`,
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

// Governance proposal status (crates/core/governance/src/status.rs).
export const PROPOSAL_STATUS = [
  "Draft",
  "Approved",
  "Rejected",
  "Rework",
  "Implemented",
] as const;

export function proposalStatusName(v: number): string {
  return PROPOSAL_STATUS[v] ?? `UNKNOWN(${v})`;
}

/** Status name (case-insensitive) -> on-chain status code. */
export function proposalStatusCode(name: string): number {
  const i = PROPOSAL_STATUS.findIndex((s) => s.toLowerCase() === name.toLowerCase());
  if (i < 0) throw new Error(`unknown proposal status "${name}"; known: ${PROPOSAL_STATUS.join(", ")}`);
  return i;
}

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
