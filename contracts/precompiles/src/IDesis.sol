// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IDesis
/// @notice Inbound call surface for the Desis runtime precompile.
///         The Desis runtime drives the auction schedule from a Metadosis brief.
///         OriginRouter calls bid ingestion. Clearing runs from the Desis
///         begin-block gate.
interface IDesis {
    error InvalidEmissionDay(uint32 emissionDay);
    error InvalidStageTransition();
    error PendingClearingDataMissing(uint32 emissionDay);
    error UnauthorizedOrigin(address value);
    error UnpricedReferenceCurrency(uint16 value);
    error RefundFanOutTooLarge(uint64 value);
    error UnspellableBidCurrency(uint16 issuance, uint16 referenceCurrency);
    error BidBatchTooLarge(uint64 count, uint64 max);
    error DesisAllocationExceedsLimit(uint32 emissionDay, uint256 allocation, uint256 limit);
    error AuctionBriefLimitExceedsDomain();
    error BatchArrayLengthMismatch();
    error BriefAnchorExceedsU32();
    error MinBidQuantityScalingOverflow();
    error TargetsOfUndecodable();
    error TooManyTargets(uint32 emissionDay, uint64 count, uint64 max);
    error BatchTotalMismatch();
    error InvalidTotalBatches();
    error InvalidBatchIndexOrTotal();
    error ScheduleTimestampExceedsU32();
    error DateStep(uint8 kind, uint32 dateKey);

    /// @notice Auction lifecycle stages. Values map 1:1 to the Rust `AuctionStage` enum.
    enum AuctionStage {
        None,
        Briefed,
        CommittingBids,
        RevealingBids,
        CollectingBids,
        Completed,
        Cancelled,
        Failed
    }

    // --- Bid ingestion (from OriginRouter) ---
    /// @notice Accept a relayed bid batch from a target chain. A day's batches may arrive in any
    ///         order over the unordered bridge. The receiver collects all `totalBatches` (by
    ///         `batchIndex`) before finalizing.
    function processBidsBatch(
        uint32 emissionDay,
        uint32 srcChainId,
        uint16 batchIndex,
        uint16 totalBatches,
        address[] calldata bidderAddresses,
        uint256[] calldata packedBids
    ) external;

    /// @notice Per-chain completeness marker: the source relayed `totalBatches`/`totalBids` for
    ///         this day. The gate clears the auction once every snapshot chain has reported (or
    ///         the fan-in deadline passes).
    function processBidsDone(uint32 emissionDay, uint32 srcChainId, uint16 totalBatches, uint32 totalBids) external;

    // --- Views ---
    function getAuctionStage(uint32 emissionDay) external view returns (AuctionStage);
    function getBidsCount(uint32 emissionDay) external view returns (uint256);
    function getChainBidsCount(uint32 emissionDay, uint32 srcChainId) external view returns (uint256);
    /// @notice Whether the chain's bid intake for the day is complete (marker + all batches
    ///         arrived).
    function isChainDone(uint32 emissionDay, uint32 srcChainId) external view returns (bool);

    /// @notice ERC-165 interface support check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);

    // --- Events ---
    event DesisLimitSet(uint32 indexed emissionDay, uint256 amountMinor);
    event DesisLimitAllocated(uint32 indexed emissionDay, uint256 amountMinor);
    event DesisLimitReturned(uint32 indexed emissionDay, uint256 amountMinor);
    event IntexPromisLoadSet(
        uint32 indexed emissionDay,
        uint128 loadMinor,
        uint16 referenceCurrency,
        uint256 rateMinor,
        uint256 referenceMinor
    );
    event IntexAuctionCreated(
        uint32 indexed emissionDay,
        uint64 commitEnd,
        uint64 revealEnd,
        uint64 issuanceEnd,
        uint128 promisLoadMinor,
        uint32 minBidRate,
        uint32 minUnits,
        uint128 bidBondMinor
    );
    event IntexAuctionCanceled(uint32 indexed emissionDay, uint8 reason);
    event IntexAuctionFailed(uint32 indexed emissionDay, uint8 stage);
    event IntexAuctionCompleted(uint32 indexed emissionDay, uint32 units, uint64 clearingRate, uint64 totalDemand);
    event IntexAuctionCompletedEmpty(uint32 indexed emissionDay, uint64 totalDemand);
    event BidsCompleted(uint32 indexed emissionDay, uint32 indexed srcChainId);
    event BidsExpired(uint32 indexed emissionDay, uint32 indexed srcChainId);
    event BidsIgnored(uint32 indexed emissionDay, uint32 indexed srcChainId, uint8 reason);
}
