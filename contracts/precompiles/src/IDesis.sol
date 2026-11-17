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
    error ReturnFanOutTooLarge(uint64 value);
    error UnspellableIntexAuctionBidCurrency(uint16 issuance, uint16 referenceCurrency);
    error IntexAuctionBidsPageTooLarge(uint64 count, uint64 max);
    error DesisAllocationExceedsLimit(uint32 emissionDay, uint256 allocation, uint256 limit);
    error PageArrayLengthMismatch();
    error DesisLimitAnchorExceedsU32();
    error MinIntexAuctionBidQuantityScalingOverflow();
    error TargetsOfUndecodable();
    error TooManyTargets(uint32 emissionDay, uint64 count, uint64 max);
    error PageTotalMismatch();
    error InvalidTotalPages();
    error InvalidPageIndexOrTotal();
    error ScheduleTimestampExceedsU32();
    error DateStep(uint8 kind, uint32 dateKey);

    /// @notice Auction lifecycle stages. Values map 1:1 to the Rust `AuctionStage` enum.
    enum AuctionStage {
        None,
        CommittingBids,
        RevealingBids,
        CollectingBids,
        Completed,
        Canceled,
        Failed
    }

    // --- Bid ingestion (from OriginRouter) ---
    /// @notice Accept a relayed bid batch from a target chain. A day's batches may arrive in any
    ///         order over the unordered bridge. The receiver collects all `totalPages` (by
    ///         `pageIndex`) before finalizing.
    function collectIntexAuctionBids(
        bytes32 receiveId,
        uint32 emissionDay,
        uint32 srcChainId,
        uint16 pageIndex,
        uint16 totalPages,
        address[] calldata bidders,
        uint256[] calldata packedBids
    ) external;

    /// @notice Per-chain completeness marker: the source relayed `totalPages`/`totalBids` for
    ///         this day. The gate clears the auction once every snapshot chain has reported (or
    ///         the fan-in deadline passes).
    function completeIntexAuctionBids(
        bytes32 receiveId,
        uint32 emissionDay,
        uint32 srcChainId,
        uint16 totalPages,
        uint32 totalBids
    ) external;

    // --- Views ---
    function auctionStage(uint32 emissionDay) external view returns (AuctionStage);
    function intexAuctionBidsCount(uint32 emissionDay) external view returns (uint256);
    function chainIntexAuctionBidsCount(uint32 emissionDay, uint32 srcChainId) external view returns (uint256);
    /// @notice Whether the chain's bids for the day are complete (totals and every page arrived).
    function isIntexAuctionBidsCompleted(uint32 emissionDay, uint32 srcChainId) external view returns (bool);

    /// @notice ERC-165 interface support check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);

    // --- Events ---
    event DesisLimitSet(uint32 indexed emissionDay, uint256 amountMinor);
    event DesisLimitAllocated(uint32 indexed emissionDay, uint256 amountMinor);
    event IntexPromisLoadSet(
        uint32 indexed emissionDay,
        uint256 unitPromisLoadMinor,
        uint16 referenceCurrency,
        uint256 entryPriceMinor,
        uint256 baseEntryPriceMinor
    );
    event DesisStarted(
        uint32 indexed emissionDay,
        uint64 commitEnd,
        uint64 revealEnd,
        uint64 issuanceEnd,
        uint256 unitPromisLoadMinor,
        uint32 minBidRate,
        uint32 minBidQuantity,
        uint256 bidBondMinor
    );
    event DesisCanceled(uint32 indexed emissionDay, uint8 reason);
    event DesisFailed(uint32 indexed emissionDay, AuctionStage stage);
    event DesisCompleted(uint32 indexed emissionDay, uint32 quantity, uint64 clearingRate, uint64 totalDemand);
    event IntexAuctionBidsCollected(
        uint32 indexed emissionDay, uint32 indexed srcChainId, uint32 pageIndex, uint32 totalPages, uint32 bidsCount
    );
    event IntexAuctionBidsCompleted(
        uint32 indexed emissionDay, uint32 indexed srcChainId, uint32 totalPages, uint32 totalBids
    );
    event IntexAuctionBidsExpired(uint32 indexed emissionDay, uint32 indexed srcChainId);
    event IntexAuctionBidsIgnored(
        bytes32 indexed receiveId, uint32 indexed emissionDay, uint32 indexed srcChainId, uint8 reason
    );
    event IntexAuctionClearingStartSendFailed(uint32 indexed emissionDay, uint32 indexed dstChainId, bytes errorData);
}
