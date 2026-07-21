// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title ITargetRouter
/// @author Outbe
/// @notice Interface for a target-chain router. Relays revealed bids to Outbe and receives auction/series messages
///         from Outbe over the protocol-agnostic ERC-7786 bridge.
/// @dev Deployed on each target chain behind a UUPS proxy. Inbound delivery arrives via
///      {ERC7786MessengerBase-receiveMessage}; the bids relay is triggered by the inbound AUCTION_STAGE_CLEARING and
///      funded from the contract's relay float. Auction messages are keyed by `worldwideDay`; series
///      (issuance/mark) messages by `seriesId`.
interface ITargetRouter {
    // --- Events ---
    /// @notice Emitted when a bids batch is sent to Outbe.
    /// @param sendId Bridge send identifier.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidsCount Number of bids sent.
    event BidsBatchSent(bytes32 indexed sendId, uint32 indexed worldwideDay, uint256 bidsCount);

    /// @notice Emitted when the BIDS_DONE completeness marker is sent to Outbe after a day's chunks.
    /// @param sendId Bridge send identifier.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param totalBatches Number of BIDS_BATCH messages relayed for this day/generation.
    /// @param totalBids Total bids relayed for this day/generation.
    event BidsDoneSent(bytes32 indexed sendId, uint32 indexed worldwideDay, uint16 totalBatches, uint32 totalBids);

    /// @notice Emitted when an auction stage message is received from Outbe.
    /// @param srcChainId Source chainId the message was authenticated against.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param stageType BridgeMsgCodec message type (3=AuctionStageStart, 4=AuctionStageClearing).
    event AuctionStageReceived(uint32 indexed srcChainId, uint32 indexed worldwideDay, uint8 stageType);

    /// @notice Emitted when an auction result is received from Outbe.
    /// @param srcChainId Source chainId the message was authenticated against.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param issuedIntexCount Number of Intex units issued.
    /// @param clearingRate Uniform clearing rate (`1e6` fixed-point).
    event AuctionResultReceived(
        uint32 indexed srcChainId, uint32 indexed worldwideDay, uint32 issuedIntexCount, uint64 clearingRate
    );

    /// @notice Emitted when issuance instructions are received from Outbe.
    /// @param srcChainId Source chainId the message was authenticated against.
    /// @param seriesId Series identifier.
    /// @param recipientsCount Number of recipients.
    event IssuanceInstructionsReceived(uint32 indexed srcChainId, uint32 indexed seriesId, uint256 recipientsCount);

    /// @notice Emitted when refund instructions are received from Outbe.
    /// @param srcChainId Source chainId the message was authenticated against.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param instructionsCount Number of finalization instructions.
    event RefundInstructionsReceived(uint32 indexed srcChainId, uint32 indexed worldwideDay, uint256 instructionsCount);

    /// @notice Emitted when a mark-called message is received from Outbe.
    /// @param srcChainId Source chainId the message was authenticated against.
    /// @param seriesId Series identifier.
    event MarkCalledReceived(uint32 indexed srcChainId, uint32 indexed seriesId);

    /// @notice Emitted when a mark-qualified message is received from Outbe.
    /// @param srcChainId Source chainId the message was authenticated against.
    /// @param seriesId Series identifier.
    event MarkQualifiedReceived(uint32 indexed srcChainId, uint32 indexed seriesId);

    /// @notice Emitted when the outbound bids relay from `_handleAuctionStageClearing` reverts and
    ///         the worldwideDay is parked for later retry via `flushPendingBidsRelay`.
    /// @param idx Index of the parked relay slot.
    /// @param worldwideDay Worldwide day (yyyymmdd) whose bids could not be forwarded.
    /// @param reason Raw revert bytes from the failed send (e.g. insufficient relay float).
    event BidsRelayDeferred(uint256 indexed idx, uint32 indexed worldwideDay, bytes reason);

    /// @notice Emitted when `flushPendingBidsRelay` successfully forwards a previously deferred relay.
    /// @param idx Index of the parked relay slot that was flushed.
    /// @param worldwideDay Worldwide day (yyyymmdd) whose bids were forwarded.
    event BidsRelayFlushed(uint256 indexed idx, uint32 indexed worldwideDay);

    /// @notice Emitted when the outbound holders bridge from `_handleMarkCalled` reverts and the
    ///         holders+amounts snapshot is parked for later retry via `flushPendingHoldersRelay`.
    /// @param idx Index of the parked relay slot.
    /// @param tokenId Token id whose holders could not be bridged.
    /// @param holdersCount Number of holders in the deferred snapshot.
    /// @param reason Raw revert bytes from the failed `systemMultiSend`.
    event HoldersRelayDeferred(uint256 indexed idx, uint256 indexed tokenId, uint256 holdersCount, bytes reason);

    /// @notice Emitted when finalized auction proceeds are routed cross-chain to the OriginRouter.
    event ProceedsRouted(uint32 indexed worldwideDay, uint256 amount);
    /// @notice Emitted when proceeds routing failed and the amount was parked for retry.
    event ProceedsRouteDeferred(uint256 indexed idx, uint32 indexed worldwideDay, uint256 amount, bytes reason);
    /// @notice Emitted when `flushPendingProceedsRoute` routed a previously deferred amount.
    event ProceedsRouteFlushed(uint256 indexed idx, uint32 indexed worldwideDay);
    /// @notice Emitted when the proceeds route (token bridge + OriginRouter) is set.
    event ProceedsRouteSet(address tokenBridge, address originRouter);

    /// @notice Emitted when `flushPendingHoldersRelay` successfully bridges a previously deferred snapshot.
    /// @param idx Index of the parked relay slot that was flushed.
    /// @param tokenId Token id whose holders were bridged.
    event HoldersRelayFlushed(uint256 indexed idx, uint256 indexed tokenId);

    /// @notice Emitted when an issuance mint is parked after a recipient's ERC-1155 hook reverts.
    event IssuanceMintDeferred(uint256 indexed idx, uint32 indexed seriesId, address indexed recipient, bytes reason);
    /// @notice Emitted when `flushPendingIssuanceMint` successfully retries a parked mint.
    event IssuanceMintFlushed(uint256 indexed idx, uint32 indexed seriesId);

    /// @notice Emitted when `sweepNative` transfers native tokens out of the contract.
    /// @param to Recipient of the swept native balance.
    /// @param amount Amount of native tokens (wei) swept.
    event NativeSwept(address indexed to, uint256 amount);

    // --- Errors ---
    /// @notice Zero address provided.
    /// @param field Field name that contains zero address.
    error ZeroAddress(string field);
    /// @notice A day's revealed bids need more than 256 batches, which the receiver's arrival mask cannot track.
    /// @param worldwideDay Worldwide day (yyyymmdd) whose relay was rejected.
    /// @param totalBatches Batch count the relay would have needed.
    error TooManyBidsBatches(uint32 worldwideDay, uint16 totalBatches);
    /// @notice Native-token sweep transfer failed.
    error NativeSweepFailed();
    /// @notice Native-token balance is insufficient for the requested sweep.
    /// @param available Current contract balance.
    /// @param requested Amount the admin attempted to sweep.
    error NativeBalanceInsufficient(uint256 available, uint256 requested);
    /// @notice Self-call shim was invoked by an external caller; only `address(this)` is allowed.
    error NotSelf();
    /// @notice `flushPendingBidsRelay` called for an index that was never enqueued.
    error NoSuchPendingBidsRelay(uint256 idx);
    /// @notice `flushPendingHoldersRelay` called for an index that was never enqueued.
    error NoSuchPendingHoldersRelay(uint256 idx);

    /// @notice No parked proceeds route at `idx`.
    error NoSuchPendingProceedsRoute(uint256 idx);
    /// @notice `flushPendingIssuanceMint` called for an index that was never enqueued.
    error NoSuchPendingIssuanceMint(uint256 idx);
    /// @notice Pending slot was already flushed; a re-flush would double-send the deferred relay.
    error AlreadyFlushed(uint256 idx);

    // --- Admin ---
    /// @notice Wire contract dependencies.
    /// @param auction Auction contract address.
    /// @param intex IntexNFT1155 contract address.
    /// @param escrowAdapter EscrowAdapter contract address.
    /// @param nftBridge IntexNFT1155Bridge address (for system bridge on markCalled).
    function wire(address auction, address intex, address escrowAdapter, address nftBridge) external;

    /// @notice Register (or clear) the matching messenger on `chainId` as an ERC-7930 interoperable address.
    /// @param chainId Destination/source chainId.
    /// @param interop ERC-7930 interoperable address (empty to clear).
    function setRemoteMessenger(uint32 chainId, bytes calldata interop) external;

    /// @notice Sweep native tokens (the relay-funded float) from the contract to an admin recipient.
    /// @param to Recipient address (must be non-zero).
    /// @param amount Amount in wei to sweep; must be ≤ contract balance.
    function sweepNative(address payable to, uint256 amount) external;
}
