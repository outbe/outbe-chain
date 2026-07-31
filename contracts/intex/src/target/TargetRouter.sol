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
import {ERC7786MessengerBase} from "../shared/ERC7786MessengerBase.sol";
import {BridgeMsgCodec} from "../shared/libs/BridgeMsgCodec.sol";
import {IntexNFT1155BridgeCodec} from "../shared/libs/IntexNFT1155BridgeCodec.sol";
import {IntexGas} from "../shared/libs/IntexGas.sol";
import {IIntexNFT1155Bridge} from "../shared/interfaces/IIntexNFT1155Bridge.sol";

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

    /// @notice Destination chainId of Outbe — the sole peer for every outbound send and the only accepted source.
    uint32 public immutable OUTBE_CHAIN_ID;

    /// @notice A bids relay parked because its outbound send reverted (e.g. relay float too low); retried via
    ///         `flushPendingBidsRelay`. Bids stay in auction state, so only the worldwideDay is snapshotted.
    struct PendingBidsRelay {
        uint32 worldwideDay;
        bool exists;
        bool done;
    }

    /// @notice A holders bridge chunk parked because `systemMultiSend` reverted; retried via
    ///         `flushPendingHoldersRelay`. markCalled does not change balances, so the snapshot stays the canonical
    ///         work. Holders migrate in `MAX_BATCH_SIZE` chunks, so each parked entry is one such chunk.
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

    /// @custom:storage-location erc7201:outbe.intex.TargetRouter
    struct TargetRouterStorage {
        /// @dev Auction contract that originates outbound bids and receives inbound stage transitions.
        IIntexAuction auction;
        /// @dev IntexNFT1155 contract that issuance, mark-called, and mark-qualified messages apply to.
        IIntexNFT1155 intex;
        /// @dev EscrowAdapter contract that refund instructions are forwarded to for finalization.
        IEscrowAdapter escrowAdapter;
        /// @dev IntexNFT1155Bridge used to bridge series holders to Outbe on markCalled.
        IIntexNFT1155Bridge nftBridge;
        /// @dev Parked BIDS_BATCH relays awaiting permissionless retry, keyed by enqueue index.
        mapping(uint256 idx => PendingBidsRelay) pendingBidsRelays;
        /// @dev Next index to assign in `pendingBidsRelays`; also the count of relays ever enqueued.
        uint256 nextPendingBidsRelayIdx;
        /// @dev Monotonic per-series counter stamped on every BIDS_BATCH send/flush. The Outbe receiver
        ///      replaces a lower generation's bids when a higher one arrives, so re-flushing a parked
        ///      relay cannot double-count demand.
        mapping(uint32 worldwideDay => uint32 generation) bidsRelayGeneration;
        /// @dev Parked holders bridges awaiting permissionless retry, keyed by enqueue index.
        mapping(uint256 idx => PendingHoldersRelay) pendingHoldersRelays;
        /// @dev Next index to assign in `pendingHoldersRelays`; also the count of bridges ever enqueued.
        uint256 nextPendingHoldersRelayIdx;
        /// @dev Parked issuance mints awaiting permissionless retry, keyed by enqueue index.
        mapping(uint256 idx => PendingIssuanceMint) pendingIssuanceMints;
        /// @dev Next index to assign in `pendingIssuanceMints`; also the count ever enqueued.
        uint256 nextPendingIssuanceMintIdx;
        /// @dev Composed-transfer token bridge that routes auction proceeds to Outbe.
        IERC7786TokenBridge tokenBridge;
        /// @dev OriginRouter address on Outbe that receives and distributes the proceeds.
        address originRouter;
        /// @dev Parked proceeds routes awaiting permissionless retry, keyed by enqueue index.
        mapping(uint256 idx => PendingProceedsRoute) pendingProceedsRoutes;
        /// @dev Next index to assign in `pendingProceedsRoutes`; also the count ever enqueued.
        uint256 nextPendingProceedsRouteIdx;
        /// @dev Set once the CLEARING for a day has triggered its bids relay, so a redelivered CLEARING never
        ///      re-relays under a fresh generation.
        mapping(uint32 worldwideDay => bool relayed) clearingRelayed;
    }

    /// @notice A proceeds route parked because its outbound send reverted (e.g. relay float too low); retried
    ///         via `flushPendingProceedsRoute`. The WCOEN is already held here, so only series+amount is snapshotted.
    struct PendingProceedsRoute {
        uint32 worldwideDay;
        uint128 amount;
        bool exists;
        bool done;
    }

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

    /// @notice IntexNFT1155 contract that issuance, mark-called, and mark-qualified messages apply to.
    function intex() external view returns (IIntexNFT1155) {
        return _ts().intex;
    }

    /// @notice EscrowAdapter contract that refund instructions are forwarded to for finalization.
    function escrowAdapter() external view returns (IEscrowAdapter) {
        return _ts().escrowAdapter;
    }

    /// @notice IntexNFT1155Bridge used to bridge series holders to Outbe on markCalled.
    function nftBridge() external view returns (IIntexNFT1155Bridge) {
        return _ts().nftBridge;
    }

    /// @notice Token bridge that routes auction proceeds to Outbe.
    function tokenBridge() external view returns (IERC7786TokenBridge) {
        return _ts().tokenBridge;
    }

    /// @notice OriginRouter address on Outbe that receives the proceeds.
    function originRouter() external view returns (address) {
        return _ts().originRouter;
    }

    /// @notice Parked proceeds route by enqueue index.
    function pendingProceedsRoutes(uint256 idx)
        external
        view
        returns (uint32 worldwideDay, uint128 amount, bool exists, bool done)
    {
        PendingProceedsRoute storage p = _ts().pendingProceedsRoutes[idx];
        return (p.worldwideDay, p.amount, p.exists, p.done);
    }

    /// @notice Parked BIDS_BATCH relay by enqueue index.
    function pendingBidsRelays(uint256 idx) external view returns (uint32 worldwideDay, bool exists, bool done) {
        PendingBidsRelay storage p = _ts().pendingBidsRelays[idx];
        return (p.worldwideDay, p.exists, p.done);
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
    /// @inheritdoc ITargetRouter
    function wire(address _auction, address _intex, address _escrowAdapter, address _nftBridge)
        external
        onlyRole(DEFAULT_ADMIN_ROLE)
    {
        if (_auction == address(0)) revert ZeroAddress("auction");
        if (_intex == address(0)) revert ZeroAddress("intex");
        if (_escrowAdapter == address(0)) revert ZeroAddress("escrowAdapter");
        if (_nftBridge == address(0)) revert ZeroAddress("nftBridge");

        TargetRouterStorage storage $ = _ts();
        $.auction = IIntexAuction(_auction);
        $.intex = IIntexNFT1155(_intex);
        $.escrowAdapter = IEscrowAdapter(_escrowAdapter);
        $.nftBridge = IIntexNFT1155Bridge(_nftBridge);
    }

    /// @inheritdoc ITargetRouter
    function setRemoteMessenger(uint32 chainId, bytes calldata interop) external onlyRole(DEFAULT_ADMIN_ROLE) {
        _setRemoteMessenger(chainId, interop);
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
            _handleAuctionStageStart(srcChainId, message);
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

    /// @notice Decode AUCTION_STAGE_START and forward the day state, schedule and params to the Auction contract.
    function _handleAuctionStageStart(uint32 _srcChainId, bytes calldata _message) internal {
        (
            uint32 worldwideDay,
            IIntexAuction.WorldwideDayState dayState,
            IIntexAuction.AuctionSchedule memory schedule,
            IIntexAuction.AuctionParams memory params
        ) = BridgeMsgCodec.decodeAuctionParams(_message);
        _ts().auction.auctionStart(worldwideDay, dayState, schedule, params);

        emit AuctionStageReceived(_srcChainId, worldwideDay, BridgeMsgCodec.MSG_AUCTION_STAGE_START);
    }

    /// @notice Decode AUCTION_STAGE_CLEARING, forward to Auction, then relay revealed bids to Outbe.
    /// @dev Only the outbound relay is caught (parked on failure); a failing inbound transition propagates so the
    ///      bridge redelivers.
    function _handleAuctionStageClearing(uint32 _srcChainId, bytes calldata _message) internal {
        TargetRouterStorage storage $ = _ts();
        uint32 worldwideDay = BridgeMsgCodec.decodeAuctionStageClearing(_message);
        $.auction.startClearingStage(worldwideDay); // idempotent; a failing transition propagates for redelivery

        // Relay the revealed bids exactly once. A redelivered CLEARING must not re-relay under a fresh generation.
        if (!$.clearingRelayed[worldwideDay]) {
            $.clearingRelayed[worldwideDay] = true;
            try this.relayBidsToOutbe(worldwideDay) {
            // ok — bids forwarded
            }
            catch (bytes memory reason) {
                uint256 idx = $.nextPendingBidsRelayIdx++;
                $.pendingBidsRelays[idx] = PendingBidsRelay({worldwideDay: worldwideDay, exists: true, done: false});
                emit BidsRelayDeferred(idx, worldwideDay, reason);
            }
        }

        emit AuctionStageReceived(_srcChainId, worldwideDay, BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING);
    }

    /// @notice Self-call shim around `_doSendBidsToOutbe`. Only callable by this contract itself —
    ///         exposing it externally would let anyone trigger relayed bids without going through
    ///         the auction-stage handler.
    /// @param worldwideDay Worldwide day (yyyymmdd) whose revealed bids are relayed to Outbe.
    function relayBidsToOutbe(uint32 worldwideDay) external {
        if (msg.sender != address(this)) revert NotSelf();
        _doSendBidsToOutbe(worldwideDay);
    }

    /// @notice Permissionless retry of a previously deferred bids relay.
    /// @param idx Index of the parked relay to flush.
    function flushPendingBidsRelay(uint256 idx) external nonReentrant {
        PendingBidsRelay storage p = _ts().pendingBidsRelays[idx];
        if (!p.exists) revert NoSuchPendingBidsRelay(idx);
        if (p.done) revert AlreadyFlushed(idx);
        p.done = true;
        _doSendBidsToOutbe(p.worldwideDay);
        emit BidsRelayFlushed(idx, p.worldwideDay);
    }

    /// @notice Fetch revealed bids from Auction and relay them to Outbe in chunked BIDS_BATCH sends.
    /// @dev Chunks of `MAX_PAYLOAD_ARRAY_LEN` share one `generation` and carry `batchIndex`/`totalBatches`, so the
    ///      unordered bridge can deliver them in any order and the receiver collects the whole generation before
    ///      finalizing. No bids → one empty batch (0 of 1) as the completion signal. Any chunk reverting reverts the
    ///      whole call, so a `flushPendingBidsRelay` retry re-sends the full set under a fresh generation.
    function _doSendBidsToOutbe(uint32 worldwideDay) internal {
        TargetRouterStorage storage $ = _ts();
        // First tuple component (AuctionData) is unused here; tuple destructure intentionally drops it.
        // slither-disable-next-line unused-return
        (, IIntexAuction.SubmittedBidData[] memory bids) = $.auction.getAuctionDetails(worldwideDay);
        uint256 bidsCount = bids.length;
        // One generation per flush; every chunk of this flush carries it so the receiver can replace
        // a prior (partial or complete) relay rather than appending to it.
        uint32 gen = ++$.bidsRelayGeneration[worldwideDay];

        if (bidsCount == 0) {
            _sendOneBidsBatch(
                worldwideDay, gen, 0, 1, new address[](0), new uint16[](0), new uint32[](0), new uint32[](0)
            );
            // Trusted bridge immutable; the flagged write is the erc7201 pointer load.
            // slither-disable-next-line reentrancy-eth
            _sendBidsDone(worldwideDay, gen, 1, 0);
            return;
        }

        uint256 maxChunk = BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN;
        uint16 totalBatches = SafeCast.toUint16((bidsCount + maxChunk - 1) / maxChunk);
        // The receiver tracks batch arrival in a 256-bit mask, so it rejects any generation with more
        // than 256 batches. Fail loudly here (the caller parks the relay) instead of sending a doomed
        // generation that the receiver drops batch-by-batch, silently excluding the whole chain-day.
        if (totalBatches > MAX_BIDS_BATCHES) revert TooManyBidsBatches(worldwideDay, totalBatches);
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
                worldwideDay, gen, batchIndex, totalBatches, bidderAddresses, intexQuantities, intexBidRates, timestamps
            );
            batchIndex++;
        }

        // Completeness marker in the same tx/generation as the chunks, so it can never outrun a lost sibling.
        // slither-disable-next-line reentrancy-eth
        _sendBidsDone(worldwideDay, gen, totalBatches, SafeCast.toUint32(bidsCount));
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
        uint16[] memory intexQuantities,
        uint32[] memory intexBidRates,
        uint32[] memory timestamps
    ) internal returns (bytes32 sendId) {
        bytes memory message = BridgeMsgCodec.encodeBidsBatch(
            worldwideDay,
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
        emit BidsBatchSent(sendId, worldwideDay, bidderAddresses.length);
    }

    /// @notice Decode AUCTION_RESULT and execute auction clearing on the Auction contract.
    function _handleAuctionResult(uint32 _srcChainId, bytes calldata _message) internal {
        (uint32 worldwideDay, uint32 issuedIntexCount, uint64 auctionClearingRate, uint32 wonBidsCount) =
            BridgeMsgCodec.decodeAuctionResult(_message);

        _ts().auction.executeAuctionClearing(worldwideDay, issuedIntexCount, auctionClearingRate, wonBidsCount);

        emit AuctionResultReceived(_srcChainId, worldwideDay, issuedIntexCount, auctionClearingRate);
    }

    /// @notice Decode ISSUANCE_INSTRUCTIONS, create the series, and mint tokens via IntexNFT1155.
    function _handleIssuanceInstructions(uint32 _srcChainId, bytes calldata _message) internal {
        TargetRouterStorage storage $ = _ts();
        BridgeMsgCodec.IssuanceInstructionsPayload memory payload = BridgeMsgCodec.decodeIssuanceInstructions(_message);

        $.intex
            .createSeries(
                IIntexNFT1155.CreateSeriesParams({
                    seriesId: payload.seriesId,
                    worldwideDay: payload.worldwideDay,
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
        (
            uint32 worldwideDay,
            address[] memory bidders,
            uint128[] memory refundedAmounts,
            uint128[] memory paidAmounts
        ) = BridgeMsgCodec.decodeRefundInstructions(_message);

        IEscrowAdapter.FinalizationInstruction[] memory instructions =
            new IEscrowAdapter.FinalizationInstruction[](bidders.length);

        for (uint256 i = 0; i < bidders.length; i++) {
            instructions[i] = IEscrowAdapter.FinalizationInstruction({
                bidder: bidders[i], refundedAmount: refundedAmounts[i], paidAmount: paidAmounts[i]
            });
        }

        uint128 totalPaid = _ts().escrowAdapter.finalizeAuction(worldwideDay, _receiveId, instructions);

        // Proceeds land here (proceedsRecipient); route them to Outbe for creator payout, parking on failure.
        if (totalPaid > 0) _routeOrParkProceeds(worldwideDay, totalPaid);

        emit RefundInstructionsReceived(_srcChainId, worldwideDay, bidders.length);
    }

    /// @notice Decode MARK_CALLED, apply it to IntexNFT1155, then bridge all series holders to Outbe.
    /// @dev On bridge failure the holders+amounts snapshot is parked for retry via
    ///      `flushPendingHoldersRelay`; markCalled itself still succeeds.
    function _handleMarkCalled(uint32 _srcChainId, bytes calldata _message) internal {
        TargetRouterStorage storage $ = _ts();
        uint32 seriesId = BridgeMsgCodec.decodeMarkCalled(_message);

        $.intex.markCalled(seriesId);

        // On the origin-as-target the holders already sit on the canonical (shared) NFT, so there is nothing to
        // migrate — only the remote targets bridge their holders back.
        if (OUTBE_CHAIN_ID != uint32(block.chainid)) {
            uint256 tokenId = $.intex.issuedTokenId(seriesId);
            (address[] memory holders, uint256[] memory amounts) = $.intex.getSeriesHoldersWithBalances(tokenId);

            // Bridge holders to Outbe in chunks of MAX_BATCH_SIZE: `systemMultiSend` caps its array at that size, so a
            // series with more holders than the cap spans several sends. Each chunk is tried and parked independently,
            // so one reverting (or over-float) chunk never blocks the rest, and a parked chunk is already within the
            // cap for `flushPendingHoldersRelay` to retry.
            uint256 maxChunk = IntexNFT1155BridgeCodec.MAX_BATCH_SIZE;
            for (uint256 start = 0; start < holders.length; start += maxChunk) {
                uint256 end = start + maxChunk;
                if (end > holders.length) end = holders.length;
                uint256 chunkLen = end - start;

                address[] memory chunkHolders = new address[](chunkLen);
                uint256[] memory chunkAmounts = new uint256[](chunkLen);
                for (uint256 i = 0; i < chunkLen; i++) {
                    chunkHolders[i] = holders[start + i];
                    chunkAmounts[i] = amounts[start + i];
                }

                try this.bridgeSeriesHoldersExt(tokenId, chunkHolders, chunkAmounts) {
                // ok — chunk forwarded
                }
                catch (bytes memory reason) {
                    uint256 idx = $.nextPendingHoldersRelayIdx++;
                    $.pendingHoldersRelays[idx] = PendingHoldersRelay({
                        tokenId: tokenId, holders: chunkHolders, amounts: chunkAmounts, exists: true, done: false
                    });
                    emit HoldersRelayDeferred(idx, tokenId, chunkLen, reason);
                }
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

    /// @notice Bridge series holders to Outbe via the IntexNFT1155Bridge's system holder migration.
    /// @dev The adapter self-funds the bridge fee from its own relay float, so no value is forwarded here.
    /// @param tokenId Token ID (series) to bridge.
    /// @param holders Source chain holder addresses.
    /// @param amounts Corresponding balances for each holder.
    function _doBridgeSeriesHolders(uint256 tokenId, address[] memory holders, uint256[] memory amounts) internal {
        // TargetRouter pays the bridge fee from its own relay float: quote it and forward it as value so the
        // universal adapter never needs to hold native. The returned sendId is informational.
        IIntexNFT1155Bridge adapter = _ts().nftBridge;
        uint256 fee = adapter.quoteSystemMultiSend(tokenId, holders, amounts, OUTBE_CHAIN_ID);
        // slither-disable-next-line unused-return,arbitrary-send-eth
        adapter.systemMultiSend{value: fee}(tokenId, holders, amounts, OUTBE_CHAIN_ID);
    }

    /// @dev Route proceeds to Outbe, parking series+amount on failure so a transport/float hiccup never rolls
    ///      back the finalization (the WCOEN is already held here). Retried via `flushPendingProceedsRoute`.
    function _routeOrParkProceeds(uint32 worldwideDay, uint128 amount) internal {
        try this.routeProceedsExt(worldwideDay, amount) {
        // ok — proceeds routed
        }
        catch (bytes memory reason) {
            TargetRouterStorage storage $ = _ts();
            uint256 idx = $.nextPendingProceedsRouteIdx++;
            $.pendingProceedsRoutes[idx] =
                PendingProceedsRoute({worldwideDay: worldwideDay, amount: amount, exists: true, done: false});
            emit ProceedsRouteDeferred(idx, worldwideDay, amount, reason);
        }
    }

    /// @notice Self-call shim around `_doRouteProceeds`. Only callable by this contract itself.
    function routeProceedsExt(uint32 worldwideDay, uint128 amount) external {
        if (msg.sender != address(this)) revert NotSelf();
        _doRouteProceeds(worldwideDay, amount);
    }

    /// @notice Permissionless retry of a previously deferred proceeds route.
    /// @param idx Index of the parked route to flush.
    function flushPendingProceedsRoute(uint256 idx) external nonReentrant {
        PendingProceedsRoute storage p = _ts().pendingProceedsRoutes[idx];
        if (!p.exists) revert NoSuchPendingProceedsRoute(idx);
        if (p.done) revert AlreadyFlushed(idx);
        p.done = true;
        _doRouteProceeds(p.worldwideDay, p.amount);
        emit ProceedsRouteFlushed(idx, p.worldwideDay);
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
