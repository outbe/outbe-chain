// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {AccessControl} from "@openzeppelin/contracts/access/AccessControl.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import {OApp, Origin, MessagingFee, MessagingReceipt} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";
import {OAppOptionsType3} from "@layerzerolabs/oapp-evm/oapp/libs/OAppOptionsType3.sol";

import {IIntexAuction} from "./interfaces/IIntexAuction.sol";
import {IIntexNFT1155} from "../shared/interfaces/IIntexNFT1155.sol";
import {IEscrowAdapter} from "./interfaces/IEscrowAdapter.sol";
import {ITargetMessenger} from "./interfaces/ITargetMessenger.sol";
import {BridgeMsgCodec} from "../shared/libs/BridgeMsgCodec.sol";
import {LzGasEstimator} from "../shared/libs/LzGasEstimator.sol";
import {IONFT1155AdapterBatch} from "../shared/interfaces/IONFT1155AdapterBatch.sol";

/// @title TargetMessenger
/// @author Outbe
/// @notice LayerZero bridge adapter for BNB Chain.
/// @dev Sends messages to Outbe, receives messages from Outbe. All auction/series
///      messages are keyed by `seriesId` (uint32).
contract TargetMessenger is ITargetMessenger, OApp, OAppOptionsType3, AccessControl, ReentrancyGuard {
    /// @notice Granted to the wired Auction contract; gates the `sendBidsBatch` outbound relay.
    bytes32 public constant AUCTION_ROLE = keccak256("AUCTION_ROLE");

    /// @notice Destination gas for inbound BIDS_BATCH: covers processBidsBatch dispatch +
    ///         auto-fired clearAuction (sort + 3 outbound LZ sends).
    /// @dev Calibrated via GasCalibration.t.sol (factory mocked, so per-item is a lower bound + headroom).
    uint128 internal constant BIDS_BASE_GAS = 1_300_000;

    /// @notice Marginal destination gas per bid (sort, storage, refund-payload slot).
    uint128 internal constant BIDS_PER_ITEM_GAS = 160_000;

    /// @notice LayerZero endpoint id of the Outbe chain that is the counterparty for every send.
    uint32 public immutable OUTBE_EID;

    /// @notice Auction contract that originates outbound bids and receives inbound stage transitions.
    IIntexAuction public auction;
    /// @notice IntexNFT1155 contract that issuance, mark-called, and mark-qualified messages apply to.
    IIntexNFT1155 public intex;
    /// @notice EscrowAdapter contract that refund instructions are forwarded to for finalization.
    IEscrowAdapter public escrowAdapter;
    /// @notice ONFT1155AdapterBatch used to bridge series holders to Outbe on markCalled.
    IONFT1155AdapterBatch public onftBatchAdapter;

    /// @notice Last inbound LayerZero nonce successfully processed for each `(srcEid, sender)` pair.
    /// @dev Backs the `nextNonce` override that switches this OApp into LayerZero's ORDERED-delivery
    ///      mode: the endpoint queries `nextNonce(srcEid, sender)` before delivery and refuses to
    ///      route any packet whose `_origin.nonce` does not equal `inboundNonce + 1`. That rejects
    ///      duplicate deliveries (same nonce twice) and out-of-order deliveries (nonce gap), both
    ///      of which would otherwise re-trigger downstream auction-stage transitions out of sequence.
    mapping(uint32 srcEid => mapping(bytes32 sender => uint64)) public inboundNonce;

    /// @notice Deferred BIDS_BATCH relay enqueued because the outbound `_lzSend` from inside
    ///         `_lzReceive` reverted (typically: pre-funded native balance too low for the LZ fee).
    /// @dev Pattern A `_handleAuctionStageClearing` runs the inbound stage transition,
    ///      then attempts to relay revealed bids to Outbe; a failure parks the seriesId here so
    ///      `flushPendingBidsRelay` can retry once the operator tops up balance. Bids themselves
    ///      stay in auction state — no need to snapshot them.
    struct PendingBidsRelay {
        uint32 seriesId;
        bool exists;
        bool done;
    }

    /// @notice Parked BIDS_BATCH relays awaiting permissionless retry, keyed by enqueue index.
    mapping(uint256 idx => PendingBidsRelay) public pendingBidsRelays;
    /// @notice Next index to assign in `pendingBidsRelays`; also the count of relays ever enqueued.
    uint256 public nextPendingBidsRelayIdx;

    /// @dev Monotonic per-series counter stamped on every BIDS_BATCH send/flush. The Outbe receiver
    ///      replaces a lower generation's bids when a higher one arrives, so re-flushing a parked
    ///      relay cannot double-count demand.
    mapping(uint32 seriesId => uint32) internal bidsRelayGeneration;

    /// @notice Deferred holders bridge enqueued because the inbound `_handleMarkCalled` could not
    ///         forward all holders+amounts via `onftBatchAdapter.systemMultiSend`.
    /// @dev Snapshot `tokenId`, `holders[]` and `amounts[]` at `_lzReceive` time — markCalled does
    ///      not change balances afterwards, so the snapshot remains the canonical work to be done.
    struct PendingHoldersRelay {
        uint256 tokenId;
        address[] holders;
        uint256[] amounts;
        bool exists;
        bool done;
    }

    /// @notice Parked holders bridges awaiting permissionless retry, keyed by enqueue index.
    mapping(uint256 idx => PendingHoldersRelay) public pendingHoldersRelays;
    /// @notice Next index to assign in `pendingHoldersRelays`; also the count of bridges ever enqueued.
    uint256 public nextPendingHoldersRelayIdx;

    constructor(address _lzEndpoint, address _delegate, uint32 _outbeEid)
        OApp(_lzEndpoint, _delegate)
        Ownable(_delegate)
    {
        _grantRole(DEFAULT_ADMIN_ROLE, _delegate);
        OUTBE_EID = _outbeEid;
    }

    // --- Admin ---
    /// @inheritdoc ITargetMessenger
    function wire(address _auction, address _intex, address _escrowAdapter, address _onftBatchAdapter)
        external
        onlyRole(DEFAULT_ADMIN_ROLE)
    {
        if (_auction == address(0)) revert ZeroAddress("auction");
        if (_intex == address(0)) revert ZeroAddress("intex");
        if (_escrowAdapter == address(0)) revert ZeroAddress("escrowAdapter");
        if (_onftBatchAdapter == address(0)) revert ZeroAddress("onftBatchAdapter");

        if (address(auction) != address(0)) _revokeRole(AUCTION_ROLE, address(auction));

        auction = IIntexAuction(_auction);
        intex = IIntexNFT1155(_intex);
        escrowAdapter = IEscrowAdapter(_escrowAdapter);
        onftBatchAdapter = IONFT1155AdapterBatch(_onftBatchAdapter);

        _grantRole(AUCTION_ROLE, _auction);
    }

    // --- Quote Functions ---
    /// @inheritdoc ITargetMessenger
    function quoteSendBidsBatch(BidsBatchParams calldata params, bool payInLzToken)
        external
        view
        returns (MessagingFee memory fee)
    {
        // Mirror `sendBidsBatch`'s message + dynamic gas sizing so the quoted fee matches the send.
        (bytes memory message, bytes memory options) = _buildBidsBatch(params, bidsRelayGeneration[params.seriesId]);
        return _quote(OUTBE_EID, message, options, payInLzToken);
    }

    // --- Send Functions ---
    /// @inheritdoc ITargetMessenger
    function sendBidsBatch(BidsBatchParams calldata params, MessagingFee calldata fee)
        external
        payable
        onlyRole(AUCTION_ROLE)
        returns (MessagingReceipt memory receipt)
    {
        uint256 len = params.bidderAddresses.length;
        if (len == 0) revert EmptyArray();
        if (
            len != params.intexQuantities.length || len != params.intexBidPrices.length
                || len != params.timestamps.length
        ) {
            revert ArrayLengthMismatch();
        }

        // One generation per send so a re-send replaces rather than double-counts on the receiver.
        // Gas option scales with bid count; the contract owns liveness sizing so the caller's
        // `params.extraOptions` is superseded for the gas dimension.
        uint32 gen = ++bidsRelayGeneration[params.seriesId];
        (bytes memory message, bytes memory options) = _buildBidsBatch(params, gen);

        receipt = _lzSend(OUTBE_EID, message, options, fee, params.refundAddress);
        emit BidsBatchSent(receipt.guid, params.seriesId, len);
    }

    /// @dev Encode a single-chunk BIDS_BATCH (`isLast = true`) for the direct send/quote path and
    ///      size its gas option to the bid count, so the quote matches the actual send byte-for-byte.
    function _buildBidsBatch(BidsBatchParams calldata params, uint32 gen)
        private
        view
        returns (bytes memory message, bytes memory options)
    {
        message = BridgeMsgCodec.encodeBidsBatch(
            params.seriesId,
            endpoint.eid(),
            true,
            gen,
            params.bidderAddresses,
            params.intexQuantities,
            params.intexBidPrices,
            params.timestamps
        );
        options = _bidsReceiveOption(params.bidderAddresses.length);
    }

    // --- Receive ---
    /// @notice LayerZero entry point for inbound messages from Outbe; advances the ORDERED nonce
    ///         and dispatches the payload, dropping a failed dispatch instead of wedging the lane.
    /// @dev Validation order: header length → per-type minimum length → dispatch. All field
    ///      slicing happens inside the per-msgType decoder where the length is already vetted.
    ///      `nonReentrant` protects against re-entry through downstream `auction` /
    ///      `escrowAdapter` / `intex` calls (e.g. a hostile NFT-receiver hook).
    /// @param _origin Source chain origin data (srcEid, sender, nonce)
    /// @param _guid Unique message identifier
    /// @param _message Encoded bridge payload
    function _lzReceive(
        Origin calldata _origin,
        bytes32 _guid,
        bytes calldata _message,
        address,
        /*_executor*/
        bytes calldata /*_extraData*/
    )
        internal
        override
        nonReentrant
    {
        // Record this packet's nonce so `nextNonce` advances by exactly one. Endpoint already
        // verified `_origin.nonce == inboundNonce + 1` before calling us; bumping here keeps the
        // invariant for the next delivery on this `(srcEid, sender)` channel.
        inboundNonce[_origin.srcEid][_origin.sender] = _origin.nonce;

        // Drop-don't-block: the nonce is already advanced, so a deterministic revert in decode or any
        // downstream transition must not escape `_lzReceive` and wedge the ORDERED lane.
        try this.dispatchInbound(_guid, _origin.srcEid, _message) {}
        catch (bytes memory reason) {
            emit InboundMessageDropped(_guid, _origin.srcEid, reason);
        }
    }

    /// @notice Self-call shim that decodes and dispatches an inbound message by msgType. Self-only so a
    ///         revert is caught in `_lzReceive` and the message dropped without wedging the ORDERED lane.
    /// @param _guid Unique message identifier
    /// @param _srcEid Source endpoint id from `_origin`
    /// @param _message Encoded bridge payload
    function dispatchInbound(bytes32 _guid, uint32 _srcEid, bytes calldata _message) external {
        if (msg.sender != address(this)) revert NotSelf();

        uint8 msgType = BridgeMsgCodec.readHeader(_message);
        BridgeMsgCodec.assertMinLength(_message, msgType);

        if (msgType == BridgeMsgCodec.MSG_AUCTION_STAGE_START) {
            _handleAuctionStageStart(_guid, _srcEid, _message);
        } else if (msgType == BridgeMsgCodec.MSG_AUCTION_STAGE_REVEAL) {
            _handleAuctionStageReveal(_guid, _srcEid, _message);
        } else if (msgType == BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING) {
            _handleAuctionStageClearing(_guid, _srcEid, _message);
        } else if (msgType == BridgeMsgCodec.MSG_AUCTION_RESULT) {
            _handleAuctionResult(_guid, _srcEid, _message);
        } else if (msgType == BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS) {
            _handleIssuanceInstructions(_guid, _srcEid, _message);
        } else if (msgType == BridgeMsgCodec.MSG_REFUND_INSTRUCTIONS) {
            _handleRefundInstructions(_guid, _srcEid, _message);
        } else if (msgType == BridgeMsgCodec.MSG_MARK_CALLED) {
            _handleMarkCalled(_guid, _srcEid, _message);
        } else if (msgType == BridgeMsgCodec.MSG_MARK_QUALIFIED) {
            _handleMarkQualified(_guid, _srcEid, _message);
        } else {
            revert BridgeMsgCodec.UnknownMsgType(msgType);
        }
    }

    /// @notice Decode AUCTION_STAGE_START and forward the schedule and params to the Auction contract.
    /// @param _guid Unique message identifier
    /// @param _srcEid Source endpoint id from `_origin`
    /// @param _message Encoded auction start payload
    function _handleAuctionStageStart(bytes32 _guid, uint32 _srcEid, bytes calldata _message) internal {
        (
            uint32 seriesId,
            uint32 commitEnd,
            uint32 revealEnd,
            uint32 issuanceEnd,
            uint128 promisLoadMinor,
            uint64 minIntexBidPrice,
            uint64 costAmountMinor,
            uint64 floorPriceMinor,
            uint16 minIntexBidQuantity
        ) = BridgeMsgCodec.decodeAuctionStageStart(_message);

        auction.auctionStart(
            seriesId,
            IIntexAuction.AuctionSchedule({commitEnd: commitEnd, revealEnd: revealEnd, issuanceEnd: issuanceEnd}),
            IIntexAuction.AuctionParams({
                promisLoadMinor: promisLoadMinor,
                minIntexBidPrice: minIntexBidPrice,
                costAmountMinor: costAmountMinor,
                floorPriceMinor: floorPriceMinor,
                minIntexBidQuantity: minIntexBidQuantity
            })
        );

        emit AuctionStageReceived(_guid, _srcEid, seriesId, BridgeMsgCodec.MSG_AUCTION_STAGE_START);
    }

    /// @notice Decode AUCTION_STAGE_REVEAL and start the revealing-bids stage on the Auction contract.
    /// @param _guid Unique message identifier
    /// @param _srcEid Source endpoint id from `_origin`
    /// @param _message Encoded reveal stage payload
    function _handleAuctionStageReveal(bytes32 _guid, uint32 _srcEid, bytes calldata _message) internal {
        (uint32 seriesId, bool isGreenDay) = BridgeMsgCodec.decodeAuctionStageReveal(_message);
        auction.startRevealingBidsStage(seriesId, isGreenDay);

        emit AuctionStageReceived(_guid, _srcEid, seriesId, BridgeMsgCodec.MSG_AUCTION_STAGE_REVEAL);
    }

    /// @notice Decode AUCTION_STAGE_CLEARING, forward to Auction, then relay revealed bids to Outbe.
    /// @dev Inbound stage transition runs first and is required to succeed; the bids relay is
    ///      attempted in a try/catch shim — if `_lzSend` reverts (e.g. low pre-funded balance),
    ///      the seriesId is parked for permissionless retry via `flushPendingBidsRelay`.
    /// @param _guid Unique message identifier
    /// @param _srcEid Source endpoint id from `_origin`
    /// @param _message Encoded clearing stage payload
    function _handleAuctionStageClearing(bytes32 _guid, uint32 _srcEid, bytes calldata _message) internal {
        uint32 seriesId = BridgeMsgCodec.decodeAuctionStageClearing(_message);
        auction.startClearingStage(seriesId);

        try this.relayBidsToOutbe(seriesId) {
        // ok — bids forwarded
        }
        catch (bytes memory reason) {
            uint256 idx = nextPendingBidsRelayIdx++;
            pendingBidsRelays[idx] = PendingBidsRelay({seriesId: seriesId, exists: true, done: false});
            emit BidsRelayDeferred(idx, seriesId, reason);
        }

        emit AuctionStageReceived(_guid, _srcEid, seriesId, BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING);
    }

    /// @notice Self-call shim around `_doSendBidsToOutbe`. Only callable by this contract itself —
    ///         exposing it externally would let anyone trigger relayed bids without going through
    ///         the auction-stage handler.
    /// @param seriesId Series identifier whose revealed bids are relayed to Outbe.
    function relayBidsToOutbe(uint32 seriesId) external {
        if (msg.sender != address(this)) revert NotSelf();
        _doSendBidsToOutbe(seriesId);
    }

    /// @notice Permissionless retry of a previously deferred bids relay.
    /// @param idx Index of the parked relay to flush.
    function flushPendingBidsRelay(uint256 idx) external nonReentrant {
        PendingBidsRelay storage p = pendingBidsRelays[idx];
        if (!p.exists) revert NoSuchPendingBidsRelay(idx);
        if (p.done) revert AlreadyFlushed(idx);
        p.done = true;
        _doSendBidsToOutbe(p.seriesId);
        emit BidsRelayFlushed(idx, p.seriesId);
    }

    /// @notice Fetch revealed bids from Auction and relay them to Outbe in chunked BIDS_BATCH sends.
    /// @dev Fetch revealed bids from Auction and relay them to Outbe, chunked into
    ///      `MAX_PAYLOAD_ARRAY_LEN`-item batches (single-chain throughput: the inbound codec caps
    ///      each batch at the same number). The final chunk is flagged `isLast`; when there are no
    ///      bids at all, one empty `isLast` batch is still sent so the receiver gets the completion
    ///      signal (the no-bid path). Uses pre-funded balance and enforcedOptions for LZ fees.
    ///
    ///      The ORDERED lane delivers chunks in send order, so the `isLast` chunk is processed last.
    ///      Every chunk's `_lzSend` runs in this one call: if any reverts (e.g. low pre-funded
    ///      balance), the whole call reverts and unwinds the earlier sends, so a retry via
    ///      `flushPendingBidsRelay` re-sends the full set without duplicating any chunk.
    /// @param seriesId Series identifier
    function _doSendBidsToOutbe(uint32 seriesId) internal {
        // First tuple component (AuctionData) is unused here; tuple destructure intentionally drops it.
        // slither-disable-next-line unused-return
        (, IIntexAuction.SubmittedBidData[] memory bids) = auction.getAuctionDetails(seriesId);
        uint256 bidsCount = bids.length;
        uint32 srcEid = endpoint.eid();
        // One generation per flush; every chunk of this flush carries it so the receiver can replace
        // a prior (partial or complete) relay rather than appending to it.
        uint32 gen = ++bidsRelayGeneration[seriesId];

        if (bidsCount == 0) {
            _sendOneBidsBatch(
                seriesId, srcEid, true, gen, new address[](0), new uint16[](0), new uint64[](0), new uint32[](0)
            );
            return;
        }

        uint256 maxChunk = BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN;
        for (uint256 start = 0; start < bidsCount; start += maxChunk) {
            uint256 end = start + maxChunk;
            if (end > bidsCount) end = bidsCount;
            uint256 chunkLen = end - start;

            address[] memory bidderAddresses = new address[](chunkLen);
            uint16[] memory intexQuantities = new uint16[](chunkLen);
            uint64[] memory intexBidPrices = new uint64[](chunkLen);
            uint32[] memory timestamps = new uint32[](chunkLen);

            for (uint256 i = 0; i < chunkLen; i++) {
                IIntexAuction.SubmittedBidData memory bid = bids[start + i];
                bidderAddresses[i] = bid.bidderAddress;
                intexQuantities[i] = bid.intexQuantity;
                intexBidPrices[i] = bid.intexBidPrice;
                timestamps[i] = bid.timestamp;
            }

            _sendOneBidsBatch(
                seriesId, srcEid, end == bidsCount, gen, bidderAddresses, intexQuantities, intexBidPrices, timestamps
            );
        }
    }

    /// @dev Encode and `_lzSend` a single BIDS_BATCH chunk to Outbe. The gas option scales with the
    ///      chunk's bid count (the destination iterates over it) so a large chunk does not OOM the
    ///      inbound handler.
    function _sendOneBidsBatch(
        uint32 seriesId,
        uint32 srcEid,
        bool isLast,
        uint32 relayGeneration,
        address[] memory bidderAddresses,
        uint16[] memory intexQuantities,
        uint64[] memory intexBidPrices,
        uint32[] memory timestamps
    ) internal {
        bytes memory message = BridgeMsgCodec.encodeBidsBatch(
            seriesId, srcEid, isLast, relayGeneration, bidderAddresses, intexQuantities, intexBidPrices, timestamps
        );
        bytes memory options = _bidsReceiveOption(bidderAddresses.length);

        MessagingFee memory fee = _quote(OUTBE_EID, message, options, false);
        MessagingReceipt memory receipt = _lzSend(OUTBE_EID, message, options, fee, address(this));
        emit BidsBatchSent(receipt.guid, seriesId, bidderAddresses.length);
    }

    /// @dev Build the destination `lzReceiveOption` sized for an inbound BIDS_BATCH of `bidCount`
    ///      bids.
    function _bidsReceiveOption(uint256 bidCount) internal pure returns (bytes memory) {
        return LzGasEstimator.receiveOption(BIDS_BASE_GAS, BIDS_PER_ITEM_GAS, bidCount);
    }

    /// @notice Decode AUCTION_RESULT and execute auction clearing on the Auction contract.
    /// @param _guid Unique message identifier
    /// @param _srcEid Source endpoint id from `_origin`
    /// @param _message Encoded auction result payload
    function _handleAuctionResult(bytes32 _guid, uint32 _srcEid, bytes calldata _message) internal {
        (uint32 seriesId, uint32 issuedIntexCount, uint64 auctionIntexClearingPrice, uint32 wonBidsCount) =
            BridgeMsgCodec.decodeAuctionResult(_message);

        auction.executeAuctionClearing(seriesId, issuedIntexCount, auctionIntexClearingPrice, wonBidsCount);

        emit AuctionResultReceived(_guid, _srcEid, seriesId, issuedIntexCount, auctionIntexClearingPrice);
    }

    /// @notice Decode ISSUANCE_INSTRUCTIONS, create the series, and mint tokens via IntexNFT1155.
    /// @param _guid Unique message identifier
    /// @param _srcEid Source endpoint id from `_origin`
    /// @param _message Encoded issuance payload (series params + recipients + quantities)
    function _handleIssuanceInstructions(bytes32 _guid, uint32 _srcEid, bytes calldata _message) internal {
        BridgeMsgCodec.IssuanceInstructionsPayload memory payload = BridgeMsgCodec.decodeIssuanceInstructions(_message);

        intex.createSeries(payload.seriesId, payload.issuedIntexCount, payload.intexCallPeriod);
        intex.mintBatch(payload.recipients, payload.quantities, payload.seriesId);

        emit IssuanceInstructionsReceived(_guid, _srcEid, payload.seriesId, payload.recipients.length);
    }

    /// @notice Decode REFUND_INSTRUCTIONS and forward finalization instructions to the EscrowAdapter.
    /// @param _guid Unique message identifier
    /// @param _srcEid Source endpoint id from `_origin`
    /// @param _message Encoded refund payload (bidders + refund/paid amounts)
    function _handleRefundInstructions(bytes32 _guid, uint32 _srcEid, bytes calldata _message) internal {
        (uint32 seriesId, address[] memory bidders, uint64[] memory refundedAmounts, uint64[] memory paidAmounts) =
            BridgeMsgCodec.decodeRefundInstructions(_message);

        IEscrowAdapter.FinalizationInstruction[] memory instructions =
            new IEscrowAdapter.FinalizationInstruction[](bidders.length);

        for (uint256 i = 0; i < bidders.length; i++) {
            instructions[i] = IEscrowAdapter.FinalizationInstruction({
                bidder: bidders[i], refundedAmount: refundedAmounts[i], paidAmount: paidAmounts[i]
            });
        }

        escrowAdapter.finalizeAuction(seriesId, _guid, instructions);

        emit RefundInstructionsReceived(_guid, _srcEid, seriesId, bidders.length);
    }

    /// @notice Decode MARK_CALLED, apply it to IntexNFT1155, then bridge all series holders to Outbe.
    /// @dev On bridge failure the holders+amounts snapshot is parked for retry via
    ///      `flushPendingHoldersRelay`; markCalled itself still succeeds.
    /// @param _guid Unique message identifier
    /// @param _srcEid Source endpoint id from `_origin`
    /// @param _message Encoded mark-called payload (seriesId only)
    function _handleMarkCalled(bytes32 _guid, uint32 _srcEid, bytes calldata _message) internal {
        uint32 seriesId = BridgeMsgCodec.decodeMarkCalled(_message);

        intex.markCalled(seriesId);

        uint256 tokenId = intex.issuedTokenId(seriesId);
        (address[] memory holders, uint256[] memory amounts) = intex.getSeriesHoldersWithBalances(tokenId);

        if (holders.length > 0) {
            try this.bridgeSeriesHoldersExt(tokenId, holders, amounts) {
            // ok — holders forwarded
            }
            catch (bytes memory reason) {
                uint256 idx = nextPendingHoldersRelayIdx++;
                pendingHoldersRelays[idx] = PendingHoldersRelay({
                    tokenId: tokenId, holders: holders, amounts: amounts, exists: true, done: false
                });
                emit HoldersRelayDeferred(idx, tokenId, holders.length, reason);
            }
        }

        emit MarkCalledReceived(_guid, _srcEid, seriesId);
    }

    /// @notice Decode MARK_QUALIFIED and apply it to IntexNFT1155.
    /// @dev Unlike markCalled, qualifying is a pure status flip (Issued -> Qualified) with no holder
    ///      migration, so there is nothing to bridge back to Outbe.
    /// @param _guid Unique message identifier
    /// @param _srcEid Source endpoint id from `_origin`
    /// @param _message Encoded mark-qualified payload (seriesId only)
    function _handleMarkQualified(bytes32 _guid, uint32 _srcEid, bytes calldata _message) internal {
        uint32 seriesId = BridgeMsgCodec.decodeMarkQualified(_message);

        intex.markQualified(seriesId);

        emit MarkQualifiedReceived(_guid, _srcEid, seriesId);
    }

    /// @notice Self-call shim around `_doBridgeSeriesHolders`. Only callable by this contract itself.
    /// @param tokenId Token id (series) whose holders are bridged.
    /// @param holders Source chain holder addresses.
    /// @param amounts Corresponding balances for each holder.
    function bridgeSeriesHoldersExt(uint256 tokenId, address[] calldata holders, uint256[] calldata amounts) external {
        if (msg.sender != address(this)) revert NotSelf();
        _doBridgeSeriesHolders(tokenId, holders, amounts);
    }

    /// @notice Permissionless retry of a previously deferred holders bridge.
    /// @param idx Index of the parked relay to flush.
    function flushPendingHoldersRelay(uint256 idx) external nonReentrant {
        PendingHoldersRelay storage p = pendingHoldersRelays[idx];
        if (!p.exists) revert NoSuchPendingHoldersRelay(idx);
        if (p.done) revert AlreadyFlushed(idx);
        p.done = true;
        _doBridgeSeriesHolders(p.tokenId, p.holders, p.amounts);
        emit HoldersRelayFlushed(idx, p.tokenId);
    }

    /// @notice Quote and execute systemMultiSend via ONFT1155AdapterBatch to bridge series holders.
    /// @dev Uses the pre-funded balance of `onftBatchAdapter` for LZ fees.
    /// @param tokenId Token ID (series) to bridge
    /// @param holders Source chain holder addresses
    /// @param amounts Corresponding balances for each holder
    function _doBridgeSeriesHolders(uint256 tokenId, address[] memory holders, uint256[] memory amounts) internal {
        bytes memory empty = "";
        MessagingFee memory fee =
            onftBatchAdapter.quoteSystemMultiSend(tokenId, holders, amounts, OUTBE_EID, empty, false);
        // `onftBatchAdapter` is admin-wired in `wire()` and is not user-controlled; the LayerZero
        // MessagingReceipt return value is informational and intentionally discarded.
        // slither-disable-next-line arbitrary-send-eth,unused-return
        onftBatchAdapter.systemMultiSend{value: fee.nativeFee}(tokenId, holders, amounts, OUTBE_EID, empty, fee);
    }

    // --- Internal helpers ---
    /// @notice Pay the native LZ fee, drawing from `msg.value` on entry calls or the pre-funded
    ///         balance on relay calls, and refunding any excess to the entry caller.
    /// @dev Split entry-funded vs relay-funded native-fee accounting.
    ///      External `sendBidsBatch` callers supply the quoted fee via `msg.value`; the relay
    ///      paths (`_doSendBidsToOutbe` / `_doBridgeSeriesHolders` invoked from inside `_lzReceive`)
    ///      run with `msg.value == 0` and must draw from the operator-managed pre-funded balance.
    ///      Conflating the two would let an entry caller's `msg.value` silently seed future relay
    ///      sends with no refund, or let an entry caller silently drain the relay budget.
    /// @param _nativeFee Required native fee amount.
    /// @return nativeFee Actual fee paid (always `_nativeFee` — the caller-supplied value if any
    ///         is forwarded to the endpoint; excess is refunded to `msg.sender`).
    function _payNative(uint256 _nativeFee) internal override returns (uint256 nativeFee) {
        if (msg.value == 0) {
            // Relay path: this call originated inside `_lzReceive` (or one of the flush*
            // helpers) — there's no caller-supplied value. Pay from the pre-funded balance.
            if (address(this).balance < _nativeFee) revert NotEnoughNative(address(this).balance);
            return _nativeFee;
        }

        // Entry path: caller supplied `msg.value` against a quoted fee.
        if (msg.value < _nativeFee) revert MsgValueBelowFee(msg.value, _nativeFee);

        uint256 refund = msg.value - _nativeFee;
        if (refund > 0) {
            // Refund excess back to the entry caller so it does not silently seed the relay budget.
            // slither-disable-next-line arbitrary-send-eth
            (bool ok,) = msg.sender.call{value: refund}("");
            if (!ok) revert RefundFailed();
        }
        return _nativeFee;
    }

    /// @inheritdoc ITargetMessenger
    function sweepNative(address payable to, uint256 amount) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (to == address(0)) revert ZeroAddress("to");
        uint256 balance = address(this).balance;
        if (amount > balance) revert NativeBalanceInsufficient(balance, amount);

        // `to` is admin-supplied; the function is gated by DEFAULT_ADMIN_ROLE so the
        // arbitrary-destination warning is by design (admin recovery).
        // slither-disable-next-line arbitrary-send-eth
        (bool ok,) = to.call{value: amount}("");
        if (!ok) revert NativeSweepFailed();
    }

    /// @notice Accept native tokens for LayerZero fees (pre-funding)
    receive() external payable {}

    /// @notice Next expected inbound nonce for ORDERED LayerZero delivery on a `(srcEid, sender)` channel.
    /// @dev Override returns `inboundNonce + 1`. The endpoint refuses to route any packet whose
    ///      `_origin.nonce` does not equal this value, so duplicates and out-of-order deliveries are
    ///      rejected at the transport layer before `_lzReceive` runs.
    /// @param _srcEid LayerZero source endpoint id of the channel.
    /// @param _sender Source sender (bytes32-encoded address) of the channel.
    /// @return The next inbound nonce the endpoint must deliver for this channel.
    function nextNonce(uint32 _srcEid, bytes32 _sender) public view override returns (uint64) {
        return inboundNonce[_srcEid][_sender] + 1;
    }

    /// @notice Check whether the contract supports a given interface (ERC-165).
    /// @param interfaceId Interface ID to check
    /// @return True if the interface is supported
    function supportsInterface(bytes4 interfaceId) public view override(AccessControl) returns (bool) {
        return super.supportsInterface(interfaceId);
    }
}
