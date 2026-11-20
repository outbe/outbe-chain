// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title ITargetRouter
/// @author Outbe
/// @notice Interface for a target-chain router. Relays revealed bids to Outbe and receives auction/series messages
///         from Outbe over the protocol-agnostic ERC-7786 bridge.
/// @dev Deployed on each target chain behind a UUPS proxy. Inbound delivery arrives via
///      {ERC7786MessengerBase-receiveMessage}. The inbound INTEX_AUCTION_CLEARING_START triggers the bids relay,
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

    /// @notice Emitted when issuance instructions are received from Outbe.
    /// @param srcChainId Source chainId the message was authenticated against.
    /// @param seriesId Series identifier.
    event IssuanceInstructionsReceived(uint32 indexed srcChainId, bytes14 indexed seriesId);

    /// @notice Emitted when a whole message arrives and the router does nothing with it.
    /// @param reason One of the `InboundReason` codes.
    event MessageIgnored(bytes32 indexed receiveId, uint32 indexed srcChainId, uint8 reason);

    /// @notice Emitted when the last of a day's issuance chunks has been applied on this chain.
    /// @param emissionDay Emission day whose issuance is complete here.
    /// @param totalPages How many chunks the day's run spanned.
    event IssuanceCompleted(uint32 indexed emissionDay, uint16 totalPages);

    /// @notice Emitted when a page of the winners arrives. The last page closes the day in the escrow.
    event IntexAuctionWinnersReceived(
        bytes32 indexed receiveId,
        uint32 indexed emissionDay,
        uint32 indexed srcChainId,
        uint32 pageIndex,
        uint32 totalPages,
        uint32 winnersCount
    );

    /// @notice Emitted when the proceeds of one page leave for Outbe, at the first try or at a retry that works.
    event IntexAuctionProceedsSent(
        bytes32 indexed sendId, uint32 indexed emissionDay, uint32 indexed pageIndex, uint256 amountMinor
    );

    /// @notice Emitted when the proceeds of one page cannot leave. The router keeps them.
    event IntexAuctionProceedsSendFailed(
        uint256 indexed idx, uint32 indexed emissionDay, uint32 pageIndex, uint256 amountMinor, bytes errorData
    );

    /// @notice Emitted when an admin sets the token transfer contract and the origin router.
    event TargetRouterProceedsRouteSet(address tokenBridge, address originRouter);

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
    /// @notice An inbound message came from a chain other than Outbe.
    error UnexpectedSourceChain(uint32 srcChainId);

    // --- Admin ---
    /// @notice Wire contract dependencies.
    /// @param intexAuction Auction contract address.
    /// @param intexContract IntexNFT1155 contract address.
    /// @param escrow EscrowAdapter contract address.
    function setDependencies(address intexAuction, address intexContract, address escrow) external;

    /// @notice Register (or clear) the matching messenger on `chainId` as an ERC-7930 interoperable address.
    /// @param chainId Destination/source chainId.
    /// @param interop ERC-7930 interoperable address (empty to clear).
    function registerRemoteMessenger(uint32 chainId, bytes calldata interop) external;
}
