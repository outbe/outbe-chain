// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {SafeCast} from "@openzeppelin/contracts/utils/math/SafeCast.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";

import {IIntexAuction} from "./interfaces/IIntexAuction.sol";
import {IIntexNFT1155} from "../shared/interfaces/IIntexNFT1155.sol";
import {IEscrowAdapter} from "./interfaces/IEscrowAdapter.sol";
import {IERC7786TokenBridge} from "./interfaces/IERC7786TokenBridge.sol";
import {ITargetRouter} from "./interfaces/ITargetRouter.sol";
import {IVwapRegistry} from "./interfaces/IVwapRegistry.sol";
import {ERC7786MessengerBase} from "../shared/ERC7786MessengerBase.sol";
import {BridgeMsgCodec} from "../shared/libs/BridgeMsgCodec.sol";
import {IntexGas} from "../shared/libs/IntexGas.sol";
import {TargetInbound} from "./libs/TargetInbound.sol";
import {
    ChunkProgress,
    ParkedMark,
    BidsRelayProgress,
    ParkedIssuance,
    ParkedProceeds,
    RefundProgress,
    TargetRouterStorage
} from "./TargetRouterStorage.sol";

/// @title TargetRouter
/// @author Outbe
/// @notice BNB-side router: sends BIDS_BATCH to Outbe and receives auction/series messages from Outbe over the
///         protocol-agnostic ERC-7786 bridge (the `crosschain` hub). The active transport is selected on the bridge.
/// @dev UUPS upgradeable behind an ERC1967 proxy; the bridge is an implementation immutable (from
///      {ERC7786MessengerBase}), so every upgrade must pass the same bridge to the constructor. All auction/series
///      auction messages are keyed by `worldwideDay`, series (issuance/mark) by `seriesId`.
contract TargetRouter is
    ITargetRouter,
    ERC7786MessengerBase,
    AccessControlUpgradeable,
    ReentrancyGuardTransient,
    UUPSUpgradeable
{
    using SafeERC20 for IERC20;

    /// @notice Max BIDS_BATCH count per relay generation; bounded by the receiver's 256-bit arrival mask.
    uint16 internal constant MAX_BIDS_BATCHES = 256;

    /// @notice Destination chainId of Outbe - the sole peer for every outbound send and the only accepted source.
    uint32 public immutable OUTBE_CHAIN_ID;

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.TargetRouter")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0x69b6aeeb915a7ddfacf9fc7eeda850d126d37a2c760f56ea4c74fddcae77ba00;

    function _ts() private pure returns (TargetRouterStorage storage $) {
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

    /// @notice IntexNFT1155 contract that issuance and mark-called messages apply to.
    function intex() external view returns (IIntexNFT1155) {
        return _ts().intex;
    }

    /// @notice EscrowAdapter contract that refund instructions are forwarded to for finalization.
    function escrowAdapter() external view returns (IEscrowAdapter) {
        return _ts().escrowAdapter;
    }

    /// @notice Token bridge that routes auction proceeds to Outbe.
    function tokenBridge() external view returns (IERC7786TokenBridge) {
        return _ts().tokenBridge;
    }

    /// @notice OriginRouter address on Outbe that receives the proceeds.
    function originRouter() external view returns (address) {
        return _ts().originRouter;
    }

    function vwapRegistry() external view returns (IVwapRegistry) {
        return _ts().vwapRegistry;
    }

    /// @notice Parked proceeds route by enqueue index.
    function parkedProceeds(uint256 idx)
        external
        view
        returns (uint32 worldwideDay, uint128 amount, bool exists, bool done)
    {
        ParkedProceeds storage p = _ts().parkedProceeds[idx];
        return (p.worldwideDay, p.amount, p.exists, p.done);
    }

    /// @notice How many proceeds routes have ever parked here; `done` in the view tells which are resolved.
    function parkedProceedsCount() external view returns (uint256) {
        return _ts().nextParkedProceedsIdx;
    }

    /// @notice How far the day's bids relay has got.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    function bidsRelay(uint32 worldwideDay) external view returns (uint16 nextBatch, uint16 totalBatches, bool done) {
        BidsRelayProgress storage p = _ts().bidsRelay[worldwideDay];
        return (p.nextBatch, p.totalBatches, p.done);
    }

    /// @notice Parked issuance at `idx`.
    function parkedIssuance(uint256 idx)
        external
        view
        returns (bytes14 seriesId, address recipient, uint256 quantity, bool exists, bool done)
    {
        ParkedIssuance storage p = _ts().parkedIssuance[idx];
        return (p.seriesId, p.recipient, p.quantity, p.exists, p.done);
    }

    /// @notice How many issuances have ever parked here; `done` in the view tells which are resolved.
    function parkedIssuanceCount() external view returns (uint256) {
        return _ts().nextParkedIssuanceIdx;
    }

    /// @notice Whether `recipient` has already been issued its allocation of `seriesId` here.
    function issued(bytes14 seriesId, address recipient) external view returns (bool) {
        return _ts().issued[seriesId][recipient];
    }

    /// @notice Issuance chunk progress of `worldwideDay` on this chain: applied so far and the declared total
    ///         (0 until the first chunk lands).
    function issuanceChunks(uint32 worldwideDay) external view returns (uint16 seen, uint16 total) {
        TargetRouterStorage storage $ = _ts();
        ChunkProgress memory progress = $.issuanceProgress[worldwideDay];
        return (progress.chunksSeen, progress.totalChunks);
    }

    /// @notice Refund chunk progress of `worldwideDay` on this chain: applied so far and the declared total
    ///         (0 until the first chunk lands).
    function refundChunks(uint32 worldwideDay) external view returns (uint16 seen, uint16 total) {
        TargetRouterStorage storage $ = _ts();
        RefundProgress memory progress = $.refundProgress[worldwideDay];
        return (progress.chunksSeen, progress.totalChunks);
    }

    /// @notice Whether issuance chunk `chunkIndex` of `worldwideDay` has been applied here.
    function issuanceChunkApplied(uint32 worldwideDay, uint16 chunkIndex) external view returns (bool) {
        return _ts().issuanceChunksApplied[worldwideDay] & (1 << chunkIndex) != 0;
    }

    /// @notice Lifecycle mark waiting for `seriesId` to land here (codec msgType, 0 = none).
    function parkedMark(bytes14 seriesId) external view returns (uint8) {
        return _ts().parkedMarks[seriesId].msgType;
    }

    // --- Admin ---
    /// @inheritdoc ITargetRouter
    function wire(address _auction, address _intex, address _escrowAdapter) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_auction == address(0)) revert ZeroAddress("auction");
        if (_intex == address(0)) revert ZeroAddress("intex");
        if (_escrowAdapter == address(0)) revert ZeroAddress("escrowAdapter");

        TargetRouterStorage storage $ = _ts();
        $.auction = IIntexAuction(_auction);
        $.intex = IIntexNFT1155(_intex);
        $.escrowAdapter = IEscrowAdapter(_escrowAdapter);
    }

    /// @inheritdoc ITargetRouter
    function setRemoteMessenger(uint32 chainId, bytes calldata interop) external onlyRole(DEFAULT_ADMIN_ROLE) {
        _setRemoteMessenger(chainId, interop);
    }

    /// @inheritdoc ITargetRouter
    function setVwapRegistry(address registry) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (registry == address(0)) revert ZeroAddress("vwapRegistry");
        address registryRouter = IVwapRegistry(registry).router();
        if (registryRouter != address(this)) revert VwapRegistryRouterMismatch(registryRouter);
        _ts().vwapRegistry = IVwapRegistry(registry);
        emit VwapRegistrySet(registry);
    }

    /// @notice Set the composed-transfer token bridge and the OriginRouter recipient for proceeds routing.
    function setProceedsRoute(address _tokenBridge, address _originRouter) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_tokenBridge == address(0)) revert ZeroAddress("tokenBridge");
        if (_originRouter == address(0)) revert ZeroAddress("originRouter");
        TargetRouterStorage storage $ = _ts();
        $.tokenBridge = IERC7786TokenBridge(_tokenBridge);
        $.originRouter = _originRouter;
        emit ProceedsRouteSet(_tokenBridge, _originRouter);
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
            TargetInbound.handleAuctionStageStart(_ts(), srcChainId, message);
        } else if (msgType == BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING) {
            TargetInbound.handleAuctionStageClearing(_ts(), srcChainId, message);
        } else if (msgType == BridgeMsgCodec.MSG_AUCTION_RESULT) {
            TargetInbound.handleAuctionResult(_ts(), srcChainId, message);
        } else if (msgType == BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS) {
            TargetInbound.handleIssuanceInstructions(_ts(), srcChainId, message);
        } else if (msgType == BridgeMsgCodec.MSG_REFUND_INSTRUCTIONS) {
            TargetInbound.handleRefundInstructions(_ts(), srcChainId, receiveId, message);
        } else if (msgType == BridgeMsgCodec.MSG_MARK_CALLED) {
            TargetInbound.handleMarkCalled(_ts(), srcChainId, message);
        } else if (msgType == BridgeMsgCodec.MSG_DAILY_VWAP) {
            TargetInbound.handleDailyVwap(_ts(), srcChainId, message);
        } else {
            revert BridgeMsgCodec.UnknownMsgType(msgType);
        }
    }

    /// @notice Self-call shim around the relay: lets an inbound delivery bound its gas and keep the stage
    ///         flip even when the relay cannot finish.
    /// @param worldwideDay Worldwide day (yyyymmdd) whose revealed bids are relayed to Outbe.
    function relayBidsToOutbe(uint32 worldwideDay) external {
        if (msg.sender != address(this)) revert NotSelf();
        _relayBids(worldwideDay);
    }

    /// @notice Self-call shim reporting an unfinished day home, isolated so a failed report cannot take
    ///         the round that just succeeded with it.
    /// @param worldwideDay Worldwide day (yyyymmdd) whose remainder is reported.
    function reportBidsRemaining(uint32 worldwideDay) external {
        if (msg.sender != address(this)) revert NotSelf();
        BidsRelayProgress storage p = _ts().bidsRelay[worldwideDay];
        bytes memory message =
            BridgeMsgCodec.encodeBidsRemaining(worldwideDay, uint32(block.chainid), p.nextBatch, p.totalBatches);
        bytes32 sendId = _send(OUTBE_CHAIN_ID, message, IntexGas.BIDS_REMAINING);
        emit BidsRemainingSent(sendId, worldwideDay, p.nextBatch, p.totalBatches);
    }

    /// @notice Permissionless push for a day whose bids have not all left, for a relay float that ran dry.
    ///         Only a day past its reveal accepts it, and that stage is set by the inbound CLEARING alone.
    /// @param worldwideDay Worldwide day (yyyymmdd) to carry on relaying.
    function relayBids(uint32 worldwideDay) external nonReentrant {
        TargetRouterStorage storage $ = _ts();
        if ($.bidsRelay[worldwideDay].done) revert NoBidsToRelay(worldwideDay);
        if ($.auction.getAuctionStage(worldwideDay) != IIntexAuction.AuctionStage.Issuance) {
            revert NoBidsToRelay(worldwideDay);
        }
        uint16 batchBefore = $.bidsRelay[worldwideDay].nextBatch;
        // Sends go to the immutable bridge, the writes after them are the relay's own progress.
        // slither-disable-next-line reentrancy-eth
        _relayBids(worldwideDay);
        _reportIfAdvanced(worldwideDay, batchBefore);
    }

    function _reportIfAdvanced(uint32 worldwideDay, uint16 batchBefore) internal {
        BidsRelayProgress storage p = _ts().bidsRelay[worldwideDay];
        if (!TargetInbound.advanced(p, batchBefore)) return;
        // solhint-disable-next-line no-empty-blocks
        try this.reportBidsRemaining(worldwideDay) {}
        catch {
            emit BidsRemainingUnreported(worldwideDay, p.nextBatch, p.totalBatches);
        }
    }

    /// @notice Relay the day's revealed bids in chunked BIDS_BATCH sends, resuming where the last round
    ///         stopped.
    /// @dev The first round freezes the span and the generation - reveals are closed by then, so a bid's
    ///      chunk never moves - and every chunk carries `batchIndex`/`totalBatches` for the unordered
    ///      bridge. The marker goes with the last chunk; no bids -> one empty batch (0 of 1).
    function _relayBids(uint32 worldwideDay) internal {
        TargetRouterStorage storage $ = _ts();
        BidsRelayProgress storage progress = $.bidsRelay[worldwideDay];
        if (progress.done) return;

        uint256 bidsCount = $.auction.revealedBidsCount(worldwideDay);
        uint16 totalBatches = progress.totalBatches;
        uint32 generation;
        if (totalBatches == 0) {
            uint256 maxChunk = BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN;
            totalBatches = bidsCount == 0 ? 1 : SafeCast.toUint16((bidsCount + maxChunk - 1) / maxChunk);
            // The receiver tracks batch arrival in a 256-bit mask, so it rejects any generation with more
            // than 256 batches. Fail here instead of sending a doomed generation it drops batch by batch.
            if (totalBatches > MAX_BIDS_BATCHES) revert TooManyBidsBatches(worldwideDay, totalBatches);
            progress.totalBatches = totalBatches;
            generation = ++$.bidsRelayGeneration[worldwideDay];
        } else {
            generation = $.bidsRelayGeneration[worldwideDay];
        }

        uint16 batch = progress.nextBatch;
        while (batch < totalBatches) {
            // The last chunk has to leave room for the marker that follows it in the same round.
            uint256 need = batch + 1 == totalBatches
                ? IntexGas.RELAY_CHUNK_GAS + IntexGas.RELAY_MARKER_GAS
                : IntexGas.RELAY_CHUNK_GAS;
            if (gasleft() <= need) break;
            _sendBidsChunk(worldwideDay, generation, batch, totalBatches, bidsCount);
            ++batch;
        }
        progress.nextBatch = batch;

        if (batch < totalBatches) {
            emit BidsRelayIncomplete(worldwideDay, batch, totalBatches);
            return;
        }

        progress.done = true;
        // Completeness marker in the same round as the last chunk, so it can never outrun a lost sibling.
        // slither-disable-next-line reentrancy-eth
        _sendBidsDone(worldwideDay, generation, totalBatches, SafeCast.toUint32(bidsCount));
        emit BidsRelayComplete(worldwideDay, totalBatches);
    }

    /// @dev Read and send one chunk: only the bids it carries are pulled from the auction.
    function _sendBidsChunk(
        uint32 worldwideDay,
        uint32 generation,
        uint16 batchIndex,
        uint16 totalBatches,
        uint256 bidsCount
    ) private {
        uint256 maxChunk = BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN;
        uint256 offset = uint256(batchIndex) * maxChunk;
        uint256 chunkLen = bidsCount > offset ? bidsCount - offset : 0;
        if (chunkLen > maxChunk) chunkLen = maxChunk;

        address[] memory bidderAddresses = new address[](chunkLen);
        uint256[] memory packedBids = new uint256[](chunkLen);
        if (chunkLen != 0) {
            IIntexAuction.SubmittedBidData[] memory bids =
                _ts().auction.revealedBidsSlice(worldwideDay, offset, chunkLen);
            for (uint256 i = 0; i < chunkLen; i++) {
                IIntexAuction.SubmittedBidData memory bid = bids[i];
                bidderAddresses[i] = bid.bidderAddress;
                packedBids[i] = BridgeMsgCodec.packBid(
                    bid.intexQuantity, bid.intexBidRate, bid.timestamp, bid.issuanceCurrency, bid.referenceCurrency
                );
            }
        }

        _sendOneBidsBatch(worldwideDay, generation, batchIndex, totalBatches, bidderAddresses, packedBids);
    }

    /// @dev Encode and `_send` the BIDS_DONE completeness marker for a day/generation. Carries this chain's chainId
    ///      as its source, cross-checked by the receiver against the authenticated source.
    function _sendBidsDone(uint32 worldwideDay, uint32 relayGeneration, uint16 totalBatches, uint32 totalBids)
        internal
    {
        bytes memory message = BridgeMsgCodec.encodeBidsDone(
            worldwideDay, uint32(block.chainid), relayGeneration, totalBatches, totalBids
        );
        bytes32 sendId = _send(OUTBE_CHAIN_ID, message, IntexGas.BIDS_DONE);
        emit BidsDoneSent(sendId, worldwideDay, totalBatches, totalBids);
    }

    /// @dev Encode and `_send` a single BIDS_BATCH to Outbe. The body carries this chain's chainId as its source
    ///      (cross-checked by the receiver against the authenticated source). Funded from the relay float.
    function _sendOneBidsBatch(
        uint32 worldwideDay,
        uint32 relayGeneration,
        uint16 batchIndex,
        uint16 totalBatches,
        address[] memory bidderAddresses,
        uint256[] memory packedBids
    ) internal returns (bytes32 sendId) {
        bytes memory message = BridgeMsgCodec.encodeBidsBatch(
            worldwideDay, uint32(block.chainid), relayGeneration, batchIndex, totalBatches, bidderAddresses, packedBids
        );
        sendId = _send(OUTBE_CHAIN_ID, message, IntexGas.bidsBatch(bidderAddresses.length));
        emit BidsBatchSent(sendId, worldwideDay, bidderAddresses.length);
    }

    /// @notice Self-call shim around a single issuance; isolates a reverting recipient hook.
    function issueOne(bytes14 seriesId, address to, uint256 quantity) external {
        if (msg.sender != address(this)) revert NotSelf();
        _ts().intex.issue(to, quantity, seriesId);
    }

    /// @notice Permissionless retry of a previously deferred issuance.
    function applyParkedIssuance(uint256 idx) external nonReentrant {
        ParkedIssuance storage p = _ts().parkedIssuance[idx];
        if (!p.exists) revert NoSuchParkedIssuance(idx);
        if (p.done) revert AlreadyResolved(idx);
        p.done = true;
        _ts().intex.issue(p.recipient, p.quantity, p.seriesId);
        emit ParkedIssuanceApplied(idx, p.seriesId);
    }

    /// @notice Self-call shim around one Called mark; isolates a series that will not take it.
    /// @param seriesId Series the mark applies to.
    /// @param calledAt Origin's call timestamp.
    function applyMarkOne(bytes14 seriesId, uint32 calledAt) external {
        if (msg.sender != address(this)) revert NotSelf();
        _ts().intex.markCalled(seriesId, calledAt);
    }

    /// @notice Permissionless apply of the mark waiting in `seriesId`'s slot. Reverts if nothing waits or the
    ///         series still will not take it, leaving the slot in place. A slot holding anything but a Called
    ///         mark is cleared without effect.
    /// @param seriesId Series whose slotted mark to apply.
    function applyParkedMark(bytes14 seriesId) external nonReentrant {
        TargetRouterStorage storage $ = _ts();
        ParkedMark memory waiting = $.parkedMarks[seriesId];
        uint8 msgType = waiting.msgType;
        if (msgType == 0) revert NoParkedMark(seriesId);
        delete $.parkedMarks[seriesId];
        if (msgType != BridgeMsgCodec.MSG_MARK_CALLED) return;
        $.intex.markCalled(seriesId, waiting.calledAt);
        emit ParkedMarkApplied(seriesId, msgType);
    }

    /// @notice Self-call shim around `_doRouteProceeds`. Only callable by this contract itself.
    function routeProceedsExt(uint32 worldwideDay, uint128 amount) external {
        if (msg.sender != address(this)) revert NotSelf();
        _doRouteProceeds(worldwideDay, amount);
    }

    /// @notice Permissionless retry of a previously deferred proceeds route.
    /// @param idx Index of the parked route to flush.
    function resendParkedProceeds(uint256 idx) external nonReentrant {
        ParkedProceeds storage p = _ts().parkedProceeds[idx];
        if (!p.exists) revert NoSuchParkedProceeds(idx);
        if (p.done) revert AlreadyResolved(idx);
        p.done = true;
        _doRouteProceeds(p.worldwideDay, p.amount);
        emit ParkedProceedsResent(idx, p.worldwideDay);
    }

    /// @dev Approve the token bridge and route `amount` WCOEN to the OriginRouter with the series id, self-funding
    ///      the bridge fee from the relay float. The credited WCOEN is unwrapped and distributed on Outbe.
    function _doRouteProceeds(uint32 worldwideDay, uint128 amount) internal {
        TargetRouterStorage storage $ = _ts();
        address to = $.originRouter;
        bytes memory extraData = abi.encode(worldwideDay);
        IERC20 token = $.escrowAdapter.paymentToken();

        token.forceApprove(address($.tokenBridge), amount);
        uint256 fee = $.tokenBridge.quoteSend(OUTBE_CHAIN_ID, to, amount, extraData, IntexGas.PROCEEDS_COMPOSE);
        // slither-disable-next-line unused-return,arbitrary-send-eth
        $.tokenBridge.sendAndCall{value: fee}(OUTBE_CHAIN_ID, to, amount, extraData, IntexGas.PROCEEDS_COMPOSE);
        emit ProceedsRouted(worldwideDay, amount);
    }

    /// @inheritdoc ITargetRouter
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
