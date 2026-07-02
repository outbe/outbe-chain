// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {SafeCast} from "@openzeppelin/contracts/utils/math/SafeCast.sol";

import {IIntexAuction} from "./interfaces/IIntexAuction.sol";
import {IIntexNFT1155} from "../shared/interfaces/IIntexNFT1155.sol";
import {IEscrowAdapter} from "./interfaces/IEscrowAdapter.sol";
import {ITargetMessenger} from "./interfaces/ITargetMessenger.sol";
import {ERC7786MessengerBase} from "../shared/ERC7786MessengerBase.sol";
import {BridgeMsgCodec} from "../shared/libs/BridgeMsgCodec.sol";
import {IntexGas} from "../shared/libs/IntexGas.sol";
import {IONFT1155AdapterBatch} from "../shared/interfaces/IONFT1155AdapterBatch.sol";

/// @title TargetMessenger
/// @author Outbe
/// @notice BNB-side messenger: sends BIDS_BATCH to Outbe and receives auction/series messages from Outbe over the
///         protocol-agnostic ERC-7786 bridge (the `crosschain` hub). The active transport is selected on the bridge.
/// @dev UUPS upgradeable behind an ERC1967 proxy; the bridge is an implementation immutable (from
///      {ERC7786MessengerBase}), so every upgrade must pass the same bridge to the constructor. All auction/series
///      messages are keyed by `seriesId` (uint32).
contract TargetMessenger is
    ITargetMessenger,
    ERC7786MessengerBase,
    AccessControlUpgradeable,
    ReentrancyGuardTransient,
    UUPSUpgradeable
{
    /// @notice Granted to the wired Auction contract; gates the `sendBidsBatch` outbound relay.
    bytes32 public constant AUCTION_ROLE = keccak256("AUCTION_ROLE");

    /// @notice Destination chainId of Outbe — the sole peer for every outbound send and the only accepted source.
    uint32 public immutable OUTBE_CHAIN_ID;

    /// @notice A bids relay parked because its outbound send reverted (e.g. relay float too low); retried via
    ///         `flushPendingBidsRelay`. Bids stay in auction state, so only the seriesId is snapshotted.
    struct PendingBidsRelay {
        uint32 seriesId;
        bool exists;
        bool done;
    }

    /// @notice A holders bridge parked because `systemMultiSend` reverted; retried via `flushPendingHoldersRelay`.
    ///         markCalled does not change balances, so the snapshot stays the canonical work.
    struct PendingHoldersRelay {
        uint256 tokenId;
        address[] holders;
        uint256[] amounts;
        bool exists;
        bool done;
    }

    /// @notice An issuance mint parked because a recipient's ERC-1155 receiver hook reverted; retried via
    ///         `flushPendingIssuanceMint`.
    struct PendingIssuanceMint {
        uint32 seriesId;
        address recipient;
        uint256 quantity;
        bool exists;
        bool done;
    }

    /// @custom:storage-location erc7201:outbe.intex.TargetMessenger
    struct TargetMessengerStorage {
        /// @dev Auction contract that originates outbound bids and receives inbound stage transitions.
        IIntexAuction auction;
        /// @dev IntexNFT1155 contract that issuance, mark-called, and mark-qualified messages apply to.
        IIntexNFT1155 intex;
        /// @dev EscrowAdapter contract that refund instructions are forwarded to for finalization.
        IEscrowAdapter escrowAdapter;
        /// @dev ONFT1155AdapterBatch used to bridge series holders to Outbe on markCalled.
        IONFT1155AdapterBatch onftBatchAdapter;
        /// @dev Parked BIDS_BATCH relays awaiting permissionless retry, keyed by enqueue index.
        mapping(uint256 idx => PendingBidsRelay) pendingBidsRelays;
        /// @dev Next index to assign in `pendingBidsRelays`; also the count of relays ever enqueued.
        uint256 nextPendingBidsRelayIdx;
        /// @dev Monotonic per-series counter stamped on every BIDS_BATCH send/flush. The Outbe receiver
        ///      replaces a lower generation's bids when a higher one arrives, so re-flushing a parked
        ///      relay cannot double-count demand.
        mapping(uint32 seriesId => uint32 generation) bidsRelayGeneration;
        /// @dev Parked holders bridges awaiting permissionless retry, keyed by enqueue index.
        mapping(uint256 idx => PendingHoldersRelay) pendingHoldersRelays;
        /// @dev Next index to assign in `pendingHoldersRelays`; also the count of bridges ever enqueued.
        uint256 nextPendingHoldersRelayIdx;
        /// @dev Parked issuance mints awaiting permissionless retry, keyed by enqueue index.
        mapping(uint256 idx => PendingIssuanceMint) pendingIssuanceMints;
        /// @dev Next index to assign in `pendingIssuanceMints`; also the count ever enqueued.
        uint256 nextPendingIssuanceMintIdx;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.TargetMessenger")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0xd3ea7ae85c719490ab42a52fee1d0107cffc5c368e656979e152d5c5183d9400;

    function _ts() private pure returns (TargetMessengerStorage storage $) {
        // solhint-disable-next-line no-inline-assembly
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address bridge_, uint32 outbeChainId_) ERC7786MessengerBase(bridge_) {
        OUTBE_CHAIN_ID = outbeChainId_;
        _disableInitializers();
    }

    /// @notice Initializes the proxy: contract admin.
    /// @param _delegate Receiver of `DEFAULT_ADMIN_ROLE`.
    function initialize(address _delegate) external initializer {
        if (_delegate == address(0)) revert ZeroAddress("delegate");
        __AccessControl_init();
        _grantRole(DEFAULT_ADMIN_ROLE, _delegate);
    }

    /// @dev Upgrades are gated by the admin role.
    // solhint-disable-next-line no-empty-blocks
    function _authorizeUpgrade(address newImplementation) internal override onlyRole(DEFAULT_ADMIN_ROLE) {}

    // --- Storage getters ---
    /// @notice Auction contract that originates outbound bids and receives inbound stage transitions.
    function auction() external view returns (IIntexAuction) {
        return _ts().auction;
    }

    /// @notice IntexNFT1155 contract that issuance, mark-called, and mark-qualified messages apply to.
    function intex() external view returns (IIntexNFT1155) {
        return _ts().intex;
    }

    /// @notice EscrowAdapter contract that refund instructions are forwarded to for finalization.
    function escrowAdapter() external view returns (IEscrowAdapter) {
        return _ts().escrowAdapter;
    }

    /// @notice ONFT1155AdapterBatch used to bridge series holders to Outbe on markCalled.
    function onftBatchAdapter() external view returns (IONFT1155AdapterBatch) {
        return _ts().onftBatchAdapter;
    }

    /// @notice Parked BIDS_BATCH relay by enqueue index.
    function pendingBidsRelays(uint256 idx) external view returns (uint32 seriesId, bool exists, bool done) {
        PendingBidsRelay storage p = _ts().pendingBidsRelays[idx];
        return (p.seriesId, p.exists, p.done);
    }

    /// @notice Next index to assign in `pendingBidsRelays`; also the count of relays ever enqueued.
    function nextPendingBidsRelayIdx() external view returns (uint256) {
        return _ts().nextPendingBidsRelayIdx;
    }

    /// @notice Parked holders bridge by enqueue index (scalar fields; arrays stay internal).
    function pendingHoldersRelays(uint256 idx) external view returns (uint256 tokenId, bool exists, bool done) {
        PendingHoldersRelay storage p = _ts().pendingHoldersRelays[idx];
        return (p.tokenId, p.exists, p.done);
    }

    /// @notice Next index to assign in `pendingHoldersRelays`; also the count of bridges ever enqueued.
    function nextPendingHoldersRelayIdx() external view returns (uint256) {
        return _ts().nextPendingHoldersRelayIdx;
    }

    /// @notice Parked issuance mint at `idx`.
    function pendingIssuanceMints(uint256 idx)
        external
        view
        returns (uint32 seriesId, address recipient, uint256 quantity, bool exists, bool done)
    {
        PendingIssuanceMint storage p = _ts().pendingIssuanceMints[idx];
        return (p.seriesId, p.recipient, p.quantity, p.exists, p.done);
    }

    /// @notice Next index to assign in `pendingIssuanceMints`.
    function nextPendingIssuanceMintIdx() external view returns (uint256) {
        return _ts().nextPendingIssuanceMintIdx;
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

        TargetMessengerStorage storage $ = _ts();
        if (address($.auction) != address(0)) _revokeRole(AUCTION_ROLE, address($.auction));

        $.auction = IIntexAuction(_auction);
        $.intex = IIntexNFT1155(_intex);
        $.escrowAdapter = IEscrowAdapter(_escrowAdapter);
        $.onftBatchAdapter = IONFT1155AdapterBatch(_onftBatchAdapter);

        _grantRole(AUCTION_ROLE, _auction);
    }

    /// @inheritdoc ITargetMessenger
    function setRemoteMessenger(uint32 chainId, bytes calldata interop) external onlyRole(DEFAULT_ADMIN_ROLE) {
        _setRemoteMessenger(chainId, interop);
    }

    // --- Quote ---
    /// @inheritdoc ITargetMessenger
    function quoteSendBidsBatch(BidsBatchParams calldata params) external view returns (uint256) {
        // Mirror `sendBidsBatch`'s single-batch encoding so the quoted fee matches the send.
        return _quoteFee(
            OUTBE_CHAIN_ID,
            BridgeMsgCodec.encodeBidsBatch(
                params.seriesId,
                uint32(block.chainid),
                _ts().bidsRelayGeneration[params.seriesId],
                0,
                1,
                params.bidderAddresses,
                params.intexQuantities,
                params.intexBidRates,
                params.timestamps
            ),
            IntexGas.bidsBatch(params.bidderAddresses.length)
        );
    }

    // --- Send ---
    /// @inheritdoc ITargetMessenger
    function sendBidsBatch(BidsBatchParams calldata params)
        external
        payable
        onlyRole(AUCTION_ROLE)
        returns (bytes32 sendId)
    {
        uint256 len = params.bidderAddresses.length;
        if (len == 0) revert EmptyArray();
        if (
            len != params.intexQuantities.length || len != params.intexBidRates.length
                || len != params.timestamps.length
        ) {
            revert ArrayLengthMismatch();
        }

        // One generation per send so a re-send replaces rather than double-counts on the receiver. A caller-supplied
        // set is a single-batch flush (index 0 of 1); the codec caps its size at `MAX_PAYLOAD_ARRAY_LEN`.
        uint32 gen = ++_ts().bidsRelayGeneration[params.seriesId];
        sendId = _sendOneBidsBatch(
            params.seriesId,
            gen,
            0,
            1,
            params.bidderAddresses,
            params.intexQuantities,
            params.intexBidRates,
            params.timestamps
        );
    }

    // --- Receive ---
    /// @inheritdoc ERC7786MessengerBase
    /// @dev nonReentrant guards against re-entry through downstream `auction`/`escrowAdapter`/`intex` calls.
    function receiveMessage(bytes32 receiveId, bytes calldata sender, bytes calldata payload)
        public
        payable
        override
        nonReentrant
        returns (bytes4)
    {
        return super.receiveMessage(receiveId, sender, payload);
    }

    /// @dev Dispatch by msgType. A premature message (prerequisite stage not applied) reverts; the bridge rolls
    ///      back and the transport redelivers once the prerequisite lands.
    function _dispatch(uint32 srcChainId, bytes32 receiveId, bytes calldata message) internal override {
        uint8 msgType = BridgeMsgCodec.readHeader(message);
        BridgeMsgCodec.assertMinLength(message, msgType);

        if (msgType == BridgeMsgCodec.MSG_AUCTION_STAGE_START) {
            _handleAuctionStageStart(srcChainId, message);
        } else if (msgType == BridgeMsgCodec.MSG_AUCTION_STAGE_REVEAL) {
            _handleAuctionStageReveal(srcChainId, message);
        } else if (msgType == BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING) {
            _handleAuctionStageClearing(srcChainId, message);
        } else if (msgType == BridgeMsgCodec.MSG_AUCTION_RESULT) {
            _handleAuctionResult(srcChainId, message);
        } else if (msgType == BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS) {
            _handleIssuanceInstructions(srcChainId, message);
        } else if (msgType == BridgeMsgCodec.MSG_REFUND_INSTRUCTIONS) {
            _handleRefundInstructions(srcChainId, receiveId, message);
        } else if (msgType == BridgeMsgCodec.MSG_MARK_CALLED) {
            _handleMarkCalled(srcChainId, message);
        } else if (msgType == BridgeMsgCodec.MSG_MARK_QUALIFIED) {
            _handleMarkQualified(srcChainId, message);
        } else {
            revert BridgeMsgCodec.UnknownMsgType(msgType);
        }
    }

    /// @notice Decode AUCTION_STAGE_START and forward the schedule and params to the Auction contract.
    function _handleAuctionStageStart(uint32 _srcChainId, bytes calldata _message) internal {
        (uint32 seriesId, IIntexAuction.AuctionSchedule memory schedule, IIntexAuction.AuctionParams memory params) =
            BridgeMsgCodec.decodeAuctionParams(_message);
        _ts().auction.auctionStart(seriesId, schedule, params);

        emit AuctionStageReceived(_srcChainId, seriesId, BridgeMsgCodec.MSG_AUCTION_STAGE_START);
    }

    /// @notice Decode AUCTION_STAGE_REVEAL and start the revealing-bids stage on the Auction contract.
    function _handleAuctionStageReveal(uint32 _srcChainId, bytes calldata _message) internal {
        (uint32 seriesId, bool isGreenDay) = BridgeMsgCodec.decodeAuctionStageReveal(_message);
        _ts().auction.startRevealingBidsStage(seriesId, isGreenDay);

        emit AuctionStageReceived(_srcChainId, seriesId, BridgeMsgCodec.MSG_AUCTION_STAGE_REVEAL);
    }

    /// @notice Decode AUCTION_STAGE_CLEARING, forward to Auction, then relay revealed bids to Outbe.
    /// @dev Only the outbound relay is caught (parked on failure); a failing inbound transition propagates so the
    ///      bridge redelivers.
    function _handleAuctionStageClearing(uint32 _srcChainId, bytes calldata _message) internal {
        TargetMessengerStorage storage $ = _ts();
        uint32 seriesId = BridgeMsgCodec.decodeAuctionStageClearing(_message);
        $.auction.startClearingStage(seriesId);

        try this.relayBidsToOutbe(seriesId) {
        // ok — bids forwarded
        }
        catch (bytes memory reason) {
            uint256 idx = $.nextPendingBidsRelayIdx++;
            $.pendingBidsRelays[idx] = PendingBidsRelay({seriesId: seriesId, exists: true, done: false});
            emit BidsRelayDeferred(idx, seriesId, reason);
        }

        emit AuctionStageReceived(_srcChainId, seriesId, BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING);
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
        PendingBidsRelay storage p = _ts().pendingBidsRelays[idx];
        if (!p.exists) revert NoSuchPendingBidsRelay(idx);
        if (p.done) revert AlreadyFlushed(idx);
        p.done = true;
        _doSendBidsToOutbe(p.seriesId);
        emit BidsRelayFlushed(idx, p.seriesId);
    }

    /// @notice Fetch revealed bids from Auction and relay them to Outbe in chunked BIDS_BATCH sends.
    /// @dev Chunks of `MAX_PAYLOAD_ARRAY_LEN` share one `generation` and carry `batchIndex`/`totalBatches`, so the
    ///      unordered bridge can deliver them in any order and the receiver collects the whole generation before
    ///      finalizing. No bids → one empty batch (0 of 1) as the completion signal. Any chunk reverting reverts the
    ///      whole call, so a `flushPendingBidsRelay` retry re-sends the full set under a fresh generation.
    function _doSendBidsToOutbe(uint32 seriesId) internal {
        TargetMessengerStorage storage $ = _ts();
        // First tuple component (AuctionData) is unused here; tuple destructure intentionally drops it.
        // slither-disable-next-line unused-return
        (, IIntexAuction.SubmittedBidData[] memory bids) = $.auction.getAuctionDetails(seriesId);
        uint256 bidsCount = bids.length;
        // One generation per flush; every chunk of this flush carries it so the receiver can replace
        // a prior (partial or complete) relay rather than appending to it.
        uint32 gen = ++$.bidsRelayGeneration[seriesId];

        if (bidsCount == 0) {
            _sendOneBidsBatch(seriesId, gen, 0, 1, new address[](0), new uint16[](0), new uint32[](0), new uint32[](0));
            return;
        }

        uint256 maxChunk = BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN;
        uint16 totalBatches = SafeCast.toUint16((bidsCount + maxChunk - 1) / maxChunk);
        uint16 batchIndex = 0;
        for (uint256 start = 0; start < bidsCount; start += maxChunk) {
            uint256 end = start + maxChunk;
            if (end > bidsCount) end = bidsCount;
            uint256 chunkLen = end - start;

            address[] memory bidderAddresses = new address[](chunkLen);
            uint16[] memory intexQuantities = new uint16[](chunkLen);
            uint32[] memory intexBidRates = new uint32[](chunkLen);
            uint32[] memory timestamps = new uint32[](chunkLen);

            for (uint256 i = 0; i < chunkLen; i++) {
                IIntexAuction.SubmittedBidData memory bid = bids[start + i];
                bidderAddresses[i] = bid.bidderAddress;
                intexQuantities[i] = bid.intexQuantity;
                intexBidRates[i] = bid.intexBidRate;
                timestamps[i] = bid.timestamp;
            }

            _sendOneBidsBatch(
                seriesId, gen, batchIndex, totalBatches, bidderAddresses, intexQuantities, intexBidRates, timestamps
            );
            batchIndex++;
        }
    }

    /// @dev Encode and `_send` a single BIDS_BATCH to Outbe. The body carries this chain's chainId as its source
    ///      (cross-checked by the receiver against the authenticated source). Funded from the relay float on the
    ///      relay path (`msg.value == 0`) or from `msg.value` on the direct `sendBidsBatch` entry.
    function _sendOneBidsBatch(
        uint32 seriesId,
        uint32 relayGeneration,
        uint16 batchIndex,
        uint16 totalBatches,
        address[] memory bidderAddresses,
        uint16[] memory intexQuantities,
        uint32[] memory intexBidRates,
        uint32[] memory timestamps
    ) internal returns (bytes32 sendId) {
        bytes memory message = BridgeMsgCodec.encodeBidsBatch(
            seriesId,
            uint32(block.chainid),
            relayGeneration,
            batchIndex,
            totalBatches,
            bidderAddresses,
            intexQuantities,
            intexBidRates,
            timestamps
        );
        sendId = _send(OUTBE_CHAIN_ID, message, IntexGas.bidsBatch(bidderAddresses.length));
        emit BidsBatchSent(sendId, seriesId, bidderAddresses.length);
    }

    /// @notice Decode AUCTION_RESULT and execute auction clearing on the Auction contract.
    function _handleAuctionResult(uint32 _srcChainId, bytes calldata _message) internal {
        (uint32 seriesId, uint32 issuedIntexCount, uint64 auctionClearingRate, uint32 wonBidsCount) =
            BridgeMsgCodec.decodeAuctionResult(_message);

        _ts().auction.executeAuctionClearing(seriesId, issuedIntexCount, auctionClearingRate, wonBidsCount);

        emit AuctionResultReceived(_srcChainId, seriesId, issuedIntexCount, auctionClearingRate);
    }

    /// @notice Decode ISSUANCE_INSTRUCTIONS, create the series, and mint tokens via IntexNFT1155.
    function _handleIssuanceInstructions(uint32 _srcChainId, bytes calldata _message) internal {
        TargetMessengerStorage storage $ = _ts();
        BridgeMsgCodec.IssuanceInstructionsPayload memory payload = BridgeMsgCodec.decodeIssuanceInstructions(_message);

        $.intex
            .createSeries(
                IIntexNFT1155.CreateSeriesParams({
                    seriesId: payload.seriesId,
                    issuanceCurrency: payload.issuanceCurrency,
                    referenceCurrency: payload.referenceCurrency,
                    issuedIntexCount: payload.issuedIntexCount,
                    promisLoadMinor: payload.promisLoadMinor,
                    entryPriceMinor: payload.entryPriceMinor,
                    floorPriceMinor: payload.floorPriceMinor,
                    callPriceMinor: payload.callPriceMinor,
                    callTrigger: IIntexNFT1155.IntexCallTrigger({
                        windowDays: payload.callWindowDays,
                        thresholdDays: payload.callThresholdDays,
                        intexCallPeriod: payload.intexCallPeriod
                    })
                })
            );
        uint256 recipientsLen = payload.recipients.length;
        for (uint256 i = 0; i < recipientsLen; i++) {
            uint256 quantity = payload.quantities[i];
            if (quantity == 0) continue;
            address recipient = payload.recipients[i];
            // Per-recipient self-call: a reverting receiver hook parks only that mint, not the whole batch.
            try this.mintIssuanceOne(payload.seriesId, recipient, quantity) {}
            catch (bytes memory reason) {
                uint256 idx = $.nextPendingIssuanceMintIdx++;
                $.pendingIssuanceMints[idx] = PendingIssuanceMint({
                    seriesId: payload.seriesId, recipient: recipient, quantity: quantity, exists: true, done: false
                });
                emit IssuanceMintDeferred(idx, payload.seriesId, recipient, reason);
            }
        }

        emit IssuanceInstructionsReceived(_srcChainId, payload.seriesId, payload.recipients.length);
    }

    /// @notice Self-call shim around a single issuance mint; isolates a reverting recipient hook.
    function mintIssuanceOne(uint32 seriesId, address to, uint256 quantity) external {
        if (msg.sender != address(this)) revert NotSelf();
        _ts().intex.mint(to, quantity, seriesId);
    }

    /// @notice Permissionless retry of a previously deferred issuance mint.
    function flushPendingIssuanceMint(uint256 idx) external nonReentrant {
        PendingIssuanceMint storage p = _ts().pendingIssuanceMints[idx];
        if (!p.exists) revert NoSuchPendingIssuanceMint(idx);
        if (p.done) revert AlreadyFlushed(idx);
        p.done = true;
        _ts().intex.mint(p.recipient, p.quantity, p.seriesId);
        emit IssuanceMintFlushed(idx, p.seriesId);
    }

    /// @notice Decode REFUND_INSTRUCTIONS and forward finalization instructions to the EscrowAdapter.
    /// @dev `receiveId` is the escrow finalization tag; escrow dedups on the series' own `finalized` flag.
    function _handleRefundInstructions(uint32 _srcChainId, bytes32 _receiveId, bytes calldata _message) internal {
        (uint32 seriesId, address[] memory bidders, uint128[] memory refundedAmounts, uint128[] memory paidAmounts) =
            BridgeMsgCodec.decodeRefundInstructions(_message);

        IEscrowAdapter.FinalizationInstruction[] memory instructions =
            new IEscrowAdapter.FinalizationInstruction[](bidders.length);

        for (uint256 i = 0; i < bidders.length; i++) {
            instructions[i] = IEscrowAdapter.FinalizationInstruction({
                bidder: bidders[i], refundedAmount: refundedAmounts[i], paidAmount: paidAmounts[i]
            });
        }

        _ts().escrowAdapter.finalizeAuction(seriesId, _receiveId, instructions);

        emit RefundInstructionsReceived(_srcChainId, seriesId, bidders.length);
    }

    /// @notice Decode MARK_CALLED, apply it to IntexNFT1155, then bridge all series holders to Outbe.
    /// @dev On bridge failure the holders+amounts snapshot is parked for retry via
    ///      `flushPendingHoldersRelay`; markCalled itself still succeeds.
    function _handleMarkCalled(uint32 _srcChainId, bytes calldata _message) internal {
        TargetMessengerStorage storage $ = _ts();
        uint32 seriesId = BridgeMsgCodec.decodeMarkCalled(_message);

        $.intex.markCalled(seriesId);

        uint256 tokenId = $.intex.issuedTokenId(seriesId);
        (address[] memory holders, uint256[] memory amounts) = $.intex.getSeriesHoldersWithBalances(tokenId);

        if (holders.length > 0) {
            try this.bridgeSeriesHoldersExt(tokenId, holders, amounts) {
            // ok — holders forwarded
            }
            catch (bytes memory reason) {
                uint256 idx = $.nextPendingHoldersRelayIdx++;
                $.pendingHoldersRelays[idx] = PendingHoldersRelay({
                    tokenId: tokenId, holders: holders, amounts: amounts, exists: true, done: false
                });
                emit HoldersRelayDeferred(idx, tokenId, holders.length, reason);
            }
        }

        emit MarkCalledReceived(_srcChainId, seriesId);
    }

    /// @notice Decode MARK_QUALIFIED and apply it to IntexNFT1155.
    /// @dev Unlike markCalled, qualifying is a pure status flip (Issued -> Qualified) with no holder
    ///      migration, so there is nothing to bridge back to Outbe.
    function _handleMarkQualified(uint32 _srcChainId, bytes calldata _message) internal {
        uint32 seriesId = BridgeMsgCodec.decodeMarkQualified(_message);

        _ts().intex.markQualified(seriesId);

        emit MarkQualifiedReceived(_srcChainId, seriesId);
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
        PendingHoldersRelay storage p = _ts().pendingHoldersRelays[idx];
        if (!p.exists) revert NoSuchPendingHoldersRelay(idx);
        if (p.done) revert AlreadyFlushed(idx);
        p.done = true;
        _doBridgeSeriesHolders(p.tokenId, p.holders, p.amounts);
        emit HoldersRelayFlushed(idx, p.tokenId);
    }

    /// @notice Bridge series holders to Outbe via the ONFT batch adapter's system holder migration.
    /// @dev The adapter self-funds the bridge fee from its own relay float, so no value is forwarded here.
    /// @param tokenId Token ID (series) to bridge.
    /// @param holders Source chain holder addresses.
    /// @param amounts Corresponding balances for each holder.
    function _doBridgeSeriesHolders(uint256 tokenId, address[] memory holders, uint256[] memory amounts) internal {
        // `onftBatchAdapter` is admin-wired in `wire()` and is not user-controlled; the returned sendId is
        // informational and intentionally discarded.
        // slither-disable-next-line unused-return
        _ts().onftBatchAdapter.systemMultiSend(tokenId, holders, amounts, OUTBE_CHAIN_ID);
    }

    /// @inheritdoc ITargetMessenger
    function sweepNative(address payable to, uint256 amount) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (to == address(0)) revert ZeroAddress("to");
        uint256 balance = address(this).balance;
        if (amount > balance) revert NativeBalanceInsufficient(balance, amount);

        // admin-only native recovery; arbitrary destination is intentional
        // slither-disable-next-line arbitrary-send-eth
        (bool ok,) = to.call{value: amount}("");
        if (!ok) revert NativeSweepFailed();

        emit NativeSwept(to, amount);
    }

    /// @notice ERC-165 support check, resolving the AccessControl interface ids.
    function supportsInterface(bytes4 interfaceId) public view override(AccessControlUpgradeable) returns (bool) {
        return super.supportsInterface(interfaceId);
    }
}
