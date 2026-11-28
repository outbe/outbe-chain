// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @dev The Desis precompile. Matches `DESIS_ADDRESS` in crates/system/common/src/addresses.rs.
address constant DESIS = 0x0000000000000000000000000000000000001016;

/// @title IDesis
/// @notice Inbound call surface for the Desis runtime precompile.
///         The Desis runtime starts the auction of each emission day by its date and takes
///         the Desis limit of the day from Metadosis.
///         The ERC-7786 hub delivers the bids. Clearing runs from the Desis
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

    function auctionStage(uint32 emissionDay) external view returns (AuctionStage);
    function intexAuctionBidsCount(uint32 emissionDay) external view returns (uint256);
    function chainIntexAuctionBidsCount(uint32 emissionDay, uint32 srcChainId) external view returns (uint256);
    /// @notice Whether the chain's bids for the day are complete (totals and every page arrived).
    function isIntexAuctionBidsCompleted(uint32 emissionDay, uint32 srcChainId) external view returns (bool);

    /// @notice ERC-165 interface support check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);

    /// @notice Takes BIDS_PAGE, BIDS_TOTALS and BIDS_PAUSE of IntexAuctionTargetRouter from the ERC-7786 hub.
    ///         Reverts only for a caller other than the hub and for a day before its reveal.
    function receiveMessage(bytes32 receiveId, bytes calldata sender, bytes calldata payload) external returns (bytes4);

    /// @notice Sends the due messages of the outbox to the target chains. Anyone may call it.
    function flush() external;

    /// @notice The first message of the outbox of `chainId`: its type and day, the messages
    ///         that wait for that chain, the failed tries of the first one and its next try.
    function outboxHead(uint32 chainId)
        external
        view
        returns (uint8 msgType, uint32 emissionDay, uint32 pending, uint32 failures, uint64 nextTry);

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

    /// @notice `sendId` is the Hyperlane message id that CrosschainBridge returned.
    event IntexAuctionStartSent(bytes32 indexed sendId, uint32 indexed emissionDay, uint32 indexed dstChainId);
    event IntexAuctionCancelSent(bytes32 indexed sendId, uint32 indexed emissionDay, uint32 indexed dstChainId);
    event IntexAuctionClearingStartSent(bytes32 indexed sendId, uint32 indexed emissionDay, uint32 indexed dstChainId);
    event IntexAuctionResultSent(
        bytes32 indexed sendId,
        uint32 indexed emissionDay,
        uint32 indexed dstChainId,
        uint32 wonQuantity,
        uint32 wonBidsCount
    );
    event IntexAuctionWinnersSent(
        bytes32 indexed sendId,
        uint32 indexed emissionDay,
        uint32 indexed dstChainId,
        uint32 pageIndex,
        uint32 totalPages,
        uint32 winnersCount
    );

    /// @notice A try to send the message failed; `failures` counts the failed tries in a row.
    event IntexAuctionStartSendFailed(
        uint32 indexed emissionDay, uint32 indexed dstChainId, uint32 failures, bytes errorData
    );
    event IntexAuctionCancelSendFailed(
        uint32 indexed emissionDay, uint32 indexed dstChainId, uint32 failures, bytes errorData
    );
    event IntexAuctionClearingStartSendFailed(
        uint32 indexed emissionDay, uint32 indexed dstChainId, uint32 failures, bytes errorData
    );
    event IntexAuctionResultSendFailed(
        uint32 indexed emissionDay, uint32 indexed dstChainId, uint32 failures, bytes errorData
    );
    event IntexAuctionWinnersSendFailed(
        uint32 indexed emissionDay, uint32 indexed dstChainId, uint32 pageIndex, uint32 failures, bytes errorData
    );
}
