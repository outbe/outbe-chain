// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title ITargetRouter
/// @author Outbe
/// @notice Interface for a target-chain router. Relays revealed bids to Outbe over the protocol-agnostic ERC-7786
///         bridge and receives auction/series messages from the Interchain Account of Outbe.
/// @dev Deployed on each target chain behind a UUPS proxy. Inbound delivery arrives via
///      {receiveFromOutbe}. The inbound INTEX_AUCTION_CLEARING_START triggers the bids relay,
///      and the contract's relay float funds it. Auction messages are keyed by `emissionDay`. Series
///      (issuance/mark) messages are keyed by `seriesId`.
interface ITargetRouter {
    // --- Events ---
    /// @notice Emitted when one page of the revealed bids leaves for Outbe.
    event IntexAuctionBidsSent(
        bytes32 indexed sendId, uint32 indexed emissionDay, uint32 pageIndex, uint32 totalPages, uint32 bidsCount
    );

    /// @notice Emitted when the totals of the bids leave, with the last page.
    event IntexAuctionBidsTotalsSent(
        bytes32 indexed sendId, uint32 indexed emissionDay, uint32 totalPages, uint32 totalBids
    );

    /// @notice Emitted when a run sent some pages and the pause message leaves. Outbe answers with the
    ///         clearing start again.
    event IntexAuctionBidsPauseSent(
        bytes32 indexed sendId, uint32 indexed emissionDay, uint32 nextPage, uint32 totalPages
    );

    /// @notice Emitted when a run sent some pages and the pause message cannot leave. Any address can then
    ///         call `sendIntexAuctionBids`.
    event IntexAuctionBidsPauseSendFailed(
        uint32 indexed emissionDay, uint32 nextPage, uint32 totalPages, bytes errorData
    );

    /// @notice Emitted when a run sends no page: too little gas, a failed send, or a clearing start with too
    ///         little gas. `errorData` is empty when no send failed.
    event IntexAuctionBidsSendFailed(uint32 indexed emissionDay, uint32 nextPage, uint32 totalPages, bytes errorData);

    /// @notice Emitted when a whole message of Outbe arrives and the router does nothing with it.
    ///         The delivery transaction also holds the Hyperlane `ProcessId` of the message.
    /// @param reason One of the `InboundReason` codes.
    event IntexAuctionMessageIgnored(uint8 indexed msgType, uint32 indexed emissionDay, uint8 reason);

    /// @notice Emitted once the last page of a day's Intex series from `srcChainId` is applied on this chain.
    event IntexSeriesAllReceived(uint32 indexed emissionDay, uint32 indexed srcChainId, uint32 totalPages);

    /// @notice Emitted when a page of the winners arrives. The last page closes the day in the escrow.
    event IntexAuctionWinnersReceived(
        uint32 indexed emissionDay, uint32 indexed srcChainId, uint32 pageIndex, uint32 totalPages, uint32 winnersCount
    );

    /// @notice Emitted when the proceeds of one page leave for Outbe, at the first try or at a retry that works.
    event IntexAuctionProceedsSent(
        bytes32 indexed sendId, uint32 indexed emissionDay, uint32 indexed pageIndex, uint256 amountMinor
    );

    /// @notice Emitted when the proceeds of one page cannot leave. The router keeps them.
    event IntexAuctionProceedsSendFailed(
        uint256 indexed idx, uint32 indexed emissionDay, uint32 pageIndex, uint256 amountMinor, bytes errorData
    );

    /// @notice Emitted when an admin sets IntexAuction, the Intex contract and the escrow. Only the new values.
    event TargetRouterDependenciesSet(address intexAuction, address intexContract, address escrow);

    /// @notice Emitted when a call notice cannot be applied to its series here, and the notice waits.
    ///         `errorData` is empty when the call left no revert data, for example while the series has not
    ///         landed. The token reports the call once applied.
    event IntexSeriesCallNoticeApplyFailed(bytes14 indexed seriesId, uint32 indexed srcChainId, bytes errorData);

    // --- Errors ---
    /// @notice Zero address provided.
    /// @param field Field name that contains zero address.
    error ZeroAddress(string field);
    /// @notice A day's revealed bids need more than 256 batches. The receiver's arrival mask cannot
    ///         track that many.
    /// @param emissionDay Emission day (yyyymmdd) whose relay was rejected.
    /// @param totalPages Batch count the relay would have needed.
    error TooManyIntexAuctionBidsPages(uint32 emissionDay, uint16 totalPages);
    /// @notice An external caller invoked the self-call shim. Only `address(this)` is allowed.
    error NotSelf();
    /// @notice `setDependencies` would move the venue off an escrow that still holds something to claim.
    error EscrowHasLiveLocks();
    /// @notice `sendIntexAuctionBids` called for a day whose relay never started or has already finished.
    /// @param emissionDay Emission day (yyyymmdd).
    error NoIntexAuctionBidsToSend(uint32 emissionDay);
    /// @notice No parked proceeds route at `idx`.
    error NoSuchFailedIntexAuctionProceedsSend(uint256 idx);
    /// @notice `retryIntexSeriesCallNoticeApply` called for a series with nothing waiting in its slot.
    error NoFailedIntexSeriesCallNoticeApply(bytes14 seriesId);
    /// @notice The parked entry at `idx` was already resolved. Another retry would repeat its effect.
    error AlreadyResolved(uint256 idx);
    /// @notice A day's proceeds are in a token the proceeds bridge does not move.
    error ProceedsTokenNotBridged(address dayToken, address bridgeToken);

    // --- Admin ---
    /// @notice Wire contract dependencies.
    /// @param intexAuction Auction contract address.
    /// @param intexContract IntexNFT1155 contract address.
    /// @param escrow EscrowAdapter contract address.
    function setDependencies(address intexAuction, address intexContract, address escrow) external;

    /// @notice A message of Desis or IntexFactory on Outbe, which the Interchain Account of Outbe on
    ///         this chain calls. Only the holder of `OUTBE_ICA_ROLE` may call it.
    function receiveFromOutbe(bytes calldata message) external;
}
