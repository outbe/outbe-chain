// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {MessagingFee, MessagingReceipt} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";

/// @title ITargetMessenger
/// @author Outbe
/// @notice Interface for the BNB Chain bridge adapter.
/// @dev Deployed on BNB Chain. Sends messages to Outbe, receives messages from Outbe.
///      Receive logic is handled internally via `_lzReceive` (called by the LayerZero Endpoint).
///      All auction/series messages are keyed by `seriesId` (uint32).
interface ITargetMessenger {
    // --- Events ---
    /// @notice Emitted when bids batch is sent to Outbe.
    /// @param guid LayerZero message GUID
    /// @param seriesId Series identifier
    /// @param bidsCount Number of bids sent
    event BidsBatchSent(bytes32 indexed guid, uint32 indexed seriesId, uint256 bidsCount);

    /// @notice Emitted when an auction stage message is received from Outbe.
    /// @param guid LayerZero message GUID
    /// @param srcEid LayerZero source endpoint id (`_origin.srcEid`)
    /// @param seriesId Series identifier
    /// @param stageType BridgeMsgCodec message type (4=AuctionStageStart, 5=AuctionStageReveal, 6=AuctionStageClearing)
    event AuctionStageReceived(bytes32 indexed guid, uint32 srcEid, uint32 indexed seriesId, uint8 stageType);

    /// @notice Emitted when auction result is received from Outbe.
    /// @param guid LayerZero message GUID
    /// @param srcEid LayerZero source endpoint id (`_origin.srcEid`)
    /// @param seriesId Series identifier
    /// @param issuedIntexCount Number of Intex units issued
    /// @param clearingRate Uniform clearing rate (`1e6` fixed-point)
    event AuctionResultReceived(
        bytes32 indexed guid, uint32 srcEid, uint32 indexed seriesId, uint32 issuedIntexCount, uint64 clearingRate
    );

    /// @notice Emitted when issuance instructions are received from Outbe.
    /// @param guid LayerZero message GUID
    /// @param srcEid LayerZero source endpoint id (`_origin.srcEid`)
    /// @param seriesId Series identifier
    /// @param recipientsCount Number of recipients
    event IssuanceInstructionsReceived(
        bytes32 indexed guid, uint32 srcEid, uint32 indexed seriesId, uint256 recipientsCount
    );

    /// @notice Emitted when refund instructions are received from Outbe.
    /// @param guid LayerZero message GUID
    /// @param srcEid LayerZero source endpoint id (`_origin.srcEid`)
    /// @param seriesId Series identifier
    /// @param instructionsCount Number of finalization instructions
    event RefundInstructionsReceived(
        bytes32 indexed guid, uint32 srcEid, uint32 indexed seriesId, uint256 instructionsCount
    );

    /// @notice Emitted when mark called message is received from Outbe.
    /// @param guid LayerZero message GUID
    /// @param srcEid LayerZero source endpoint id (`_origin.srcEid`)
    /// @param seriesId Series identifier
    event MarkCalledReceived(bytes32 indexed guid, uint32 srcEid, uint32 indexed seriesId);

    /// @notice Emitted when mark qualified message is received from Outbe.
    /// @param guid LayerZero message GUID
    /// @param srcEid LayerZero source endpoint id (`_origin.srcEid`)
    /// @param seriesId Series identifier
    event MarkQualifiedReceived(bytes32 indexed guid, uint32 srcEid, uint32 indexed seriesId);

    /// @notice Emitted when the outbound bids relay from `_handleAuctionStageClearing` reverts and
    ///         the seriesId is parked for later retry via `flushPendingBidsRelay`.
    /// @param idx Index of the parked relay slot.
    /// @param seriesId Series identifier whose bids could not be forwarded.
    /// @param reason Raw revert bytes from the failed `_lzSend` (e.g. insufficient native balance).
    event BidsRelayDeferred(uint256 indexed idx, uint32 indexed seriesId, bytes reason);

    /// @notice Emitted when `flushPendingBidsRelay` successfully forwards a previously deferred relay.
    /// @param idx Index of the parked relay slot that was flushed.
    /// @param seriesId Series identifier whose bids were forwarded.
    event BidsRelayFlushed(uint256 indexed idx, uint32 indexed seriesId);

    /// @notice Emitted when the outbound holders bridge from `_handleMarkCalled` reverts and the
    ///         holders+amounts snapshot is parked for later retry via `flushPendingHoldersRelay`.
    /// @param idx Index of the parked relay slot.
    /// @param tokenId Token id whose holders could not be bridged.
    /// @param holdersCount Number of holders in the deferred snapshot.
    /// @param reason Raw revert bytes from the failed `systemMultiSend`.
    event HoldersRelayDeferred(uint256 indexed idx, uint256 indexed tokenId, uint256 holdersCount, bytes reason);

    /// @notice Emitted when `flushPendingHoldersRelay` successfully bridges a previously deferred snapshot.
    /// @param idx Index of the parked relay slot that was flushed.
    /// @param tokenId Token id whose holders were bridged.
    event HoldersRelayFlushed(uint256 indexed idx, uint256 indexed tokenId);

    /// @notice Emitted when an inbound message reverts during decode or dispatch and is dropped so the
    ///         ORDERED lane keeps advancing. The nonce has already moved; the message is not retried.
    /// @param guid LayerZero message GUID.
    /// @param srcEid LayerZero source endpoint id.
    /// @param reason Raw revert bytes from the dropped dispatch.
    event InboundMessageDropped(bytes32 indexed guid, uint32 indexed srcEid, bytes reason);

    // --- Types ---
    /// @notice Parameters for sending bids batch to Outbe
    struct BidsBatchParams {
        uint32 seriesId;
        address[] bidderAddresses;
        uint16[] intexQuantities;
        uint32[] intexBidRates;
        uint32[] timestamps;
        bytes extraOptions;
        address refundAddress;
    }

    // --- Errors ---
    /// @notice Zero address provided.
    /// @param field Field name that contains zero address
    error ZeroAddress(string field);
    /// @notice Array lengths do not match.
    error ArrayLengthMismatch();
    /// @notice Empty array provided.
    error EmptyArray();
    /// @notice Native-token sweep transfer failed.
    error NativeSweepFailed();
    /// @notice Native-token balance is insufficient for the requested sweep.
    /// @param available Current contract balance
    /// @param requested Amount the admin attempted to sweep
    error NativeBalanceInsufficient(uint256 available, uint256 requested);
    /// @notice Self-call shim was invoked by an external caller; only `address(this)` is allowed.
    error NotSelf();
    /// @notice `flushPendingBidsRelay` called for an index that was never enqueued.
    error NoSuchPendingBidsRelay(uint256 idx);
    /// @notice `flushPendingHoldersRelay` called for an index that was never enqueued.
    error NoSuchPendingHoldersRelay(uint256 idx);
    /// @notice Pending slot was already flushed; a re-flush would double-send the deferred relay.
    error AlreadyFlushed(uint256 idx);
    /// @notice External entry-funded call supplied less native than the quoted LZ fee.
    /// @param msgValue Native supplied by the caller (`msg.value`).
    /// @param nativeFee Quoted LZ fee required for the send.
    error MsgValueBelowFee(uint256 msgValue, uint256 nativeFee);
    /// @notice Refund of an excess `msg.value` back to the entry caller failed.
    error RefundFailed();

    // --- Admin ---
    /// @notice Wire contract dependencies.
    /// @param auction Auction contract address
    /// @param intex IntexNFT1155 contract address
    /// @param escrowAdapter EscrowAdapter contract address
    /// @param onftBatchAdapter ONFT1155AdapterBatch address (for system bridge on markCalled)
    function wire(address auction, address intex, address escrowAdapter, address onftBatchAdapter) external;

    /// @notice Sweep pre-funded native tokens back to an admin-chosen recipient.
    /// @dev Used to recover residual balance after LayerZero fee changes or pre-funding mistakes.
    /// @param to Recipient address (must be non-zero)
    /// @param amount Amount in wei to sweep; must be ≤ contract balance
    function sweepNative(address payable to, uint256 amount) external;

    // --- Send Functions ---
    /// @notice Quote fee for sending bids batch.
    /// @param params Bids batch parameters
    /// @param payInLzToken Whether to pay fee in LZ token
    /// @return fee Messaging fee quote
    function quoteSendBidsBatch(BidsBatchParams calldata params, bool payInLzToken)
        external
        view
        returns (MessagingFee memory fee);

    /// @notice Send bids batch to Outbe. Only callable by Auction.
    /// @param params Bids batch parameters
    /// @param fee Messaging fee
    /// @return receipt Messaging receipt
    function sendBidsBatch(BidsBatchParams calldata params, MessagingFee calldata fee)
        external
        payable
        returns (MessagingReceipt memory receipt);
}
