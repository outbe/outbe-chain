// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";
import {SafeCast} from "@openzeppelin/contracts/utils/math/SafeCast.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {ERC7786MessengerBase} from "../shared/ERC7786MessengerBase.sol";
import {IOriginRouter} from "./interfaces/IOriginRouter.sol";
import {IDesis} from "@precompiles/IDesis.sol";
import {IIntexFactory} from "@precompiles/IIntexFactory.sol";
import {IWCOEN} from "@tokens/interfaces/IWCOEN.sol";
import {IERC7786TokenReceiver} from "@tokens/interfaces/IERC7786TokenReceiver.sol";
import {BridgeMsgCodec} from "../shared/libs/BridgeMsgCodec.sol";
import {IntexGas} from "../shared/libs/IntexGas.sol";
import {InboundReason} from "../shared/libs/InboundReason.sol";

/// @title OriginRouter
/// @author Outbe
/// @notice Outbe-side router. It broadcasts auction/series messages to every registered target
///         chain. It receives BIDS_BATCH / BIDS_TOTALS back from each over the protocol-agnostic
///         ERC-7786 bridge (the `crosschain` hub). The active transport is selected on the bridge.
/// @dev UUPS upgradeable behind an ERC1967 proxy. The bridge is an implementation immutable (from
///      {ERC7786MessengerBase}), so every upgrade must pass the same bridge to the constructor.
///      Auction messages are keyed by `emissionDay`, series (issuance/mark) by `seriesId`. The
///      target set is a registry snapshotted per day at STAGE_START. Every leg is isolated, so one
///      failing destination never wedges the fan-out.
contract OriginRouter is
    IOriginRouter,
    IERC7786TokenReceiver,
    ERC7786MessengerBase,
    AccessControlUpgradeable,
    ReentrancyGuardTransient,
    UUPSUpgradeable
{
    /// @notice Gates the demand-side sends: auction stages, INTEX_AUCTION_RESULT, REFUND_INSTRUCTIONS.
    bytes32 public constant DESIS_ROLE = keccak256("DESIS_ROLE");
    /// @notice Gates the supply-side sends: ISSUANCE_INSTRUCTIONS, MARK_CALLED.
    bytes32 public constant INTEX_FACTORY_ROLE = keccak256("INTEX_FACTORY_ROLE");
    /// @dev The Desis work budgets are measured with this many targets.
    uint256 internal constant MAX_TARGETS = 16;

    /// @custom:storage-location erc7201:outbe.intex.OriginRouter
    struct OriginRouterStorage {
        /// @dev Desis recipient that processes inbound BIDS_BATCH payloads (and holds
        ///      `DESIS_ROLE`).
        address desis;
        /// @dev IntexFactory authorized for the supply-side sends (holds `INTEX_FACTORY_ROLE`).
        address intexFactory;
        /// @dev WCOEN token bridge authorized to invoke the proceeds hook.
        address tokenBridge;
        /// @dev WCOEN token unwrapped to native before distribution.
        address wcoen;
        /// @dev Parked distributions awaiting permissionless retry, by enqueue index.
        mapping(uint256 idx => FailedIntexAuctionProceedsReceive) failedIntexAuctionProceedsReceive;
        /// @dev Next index to assign in `failedIntexAuctionProceedsReceive`.
        uint256 nextFailedIntexAuctionProceedsReceiveIdx;
        // --- Multi-target registry ---
        /// @dev Registered target chainIds. `targetIndexPlus1` records membership.
        uint32[] targetChainIds;
        /// @dev 1-based index in `targetChainIds` (0 = absent). The 1-based index disambiguates the
        ///      first target under swap-pop.
        mapping(uint32 chainId => uint256 indexPlus1) targetIndexPlus1;
        /// @dev Per-day target snapshot frozen at STAGE_START. The day's sends fan out over this,
        ///      not over the live registry.
        mapping(uint32 emissionDay => uint32[] chainIds) seriesTargets;
        /// @dev Outbound legs that failed to dispatch, awaiting a permissionless flush.
        mapping(uint256 idx => FailedMessageSend) failedMessageSends;
        /// @dev Next index to assign in `failedMessageSends`.
        uint256 nextFailedMessageSendIdx;
        /// @dev Gas a day's COLLECTING_BIDS round asks of each chain, as desis sized it from that
        ///      chain's recent bid counts. Later rounds of the same day reuse it, so a remainder
        ///      report needs no sizing.
        mapping(uint32 emissionDay => mapping(uint32 chainId => uint64 gasLimit)) clearingGas;
        /// @dev Days whose cancel was sent, so a kept START of the day is never retried.
        mapping(uint32 emissionDay => bool) intexAuctionCanceled;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.OriginRouter")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0x2c9073d8b0cd30aa4aa3061d90da176c3d040651b0847724f0e9a4a76777f100;

    function _os() private pure returns (OriginRouterStorage storage $) {
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address bridge_) ERC7786MessengerBase(bridge_) {
        _disableInitializers();
    }

    /// @notice Initializes the proxy: contract admin.
    /// @param _delegate Receiver of `DEFAULT_ADMIN_ROLE`.
    function initialize(address _delegate) external initializer {
        if (_delegate == address(0)) revert ZeroAddress("delegate");
        __AccessControl_init();
        _grantRole(DEFAULT_ADMIN_ROLE, _delegate);
    }

    /// @dev The admin role gates upgrades.
    function _authorizeUpgrade(address newImplementation) internal override onlyRole(DEFAULT_ADMIN_ROLE) {}

    // --- Storage getters ---
    /// @notice Desis recipient that processes inbound BIDS_BATCH payloads (and holds `DESIS_ROLE`).
    function desis() external view returns (address) {
        return _os().desis;
    }

    /// @notice IntexFactory authorized for the supply-side sends (holds `INTEX_FACTORY_ROLE`).
    function intexFactory() external view returns (address) {
        return _os().intexFactory;
    }

    // --- Admin ---
    /// @inheritdoc IOriginRouter
    function setDependencies(address _desis, address _intexFactory) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_desis == address(0)) revert ZeroAddress("desis");
        if (_intexFactory == address(0)) revert ZeroAddress("intexFactory");
        _assertDesisInterface(_desis);

        OriginRouterStorage storage $ = _os();
        if ($.desis != address(0)) _revokeRole(DESIS_ROLE, $.desis);
        if ($.intexFactory != address(0)) _revokeRole(INTEX_FACTORY_ROLE, $.intexFactory);

        $.desis = _desis;
        $.intexFactory = _intexFactory;

        _grantRole(DESIS_ROLE, _desis);
        _grantRole(INTEX_FACTORY_ROLE, _intexFactory);

        emit OriginRouterDependenciesSet(_desis, _intexFactory);
    }

    /// @inheritdoc IOriginRouter
    function registerRemoteMessenger(uint32 chainId, bytes calldata interop) external onlyRole(DEFAULT_ADMIN_ROLE) {
        _registerRemoteMessenger(chainId, interop);
    }

    // --- Targets registry ---
    /// @inheritdoc IOriginRouter
    function addTarget(uint32 chainId) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (chainId == 0) revert ZeroChainId();
        // Require the peer messenger first, so a registered target is always routable.
        _remoteMessenger(chainId);
        OriginRouterStorage storage $ = _os();
        if ($.targetIndexPlus1[chainId] != 0) revert TargetAlreadyRegistered(chainId);
        if ($.targetChainIds.length == MAX_TARGETS) revert TooManyTargets(MAX_TARGETS);
        $.targetChainIds.push(chainId);
        $.targetIndexPlus1[chainId] = $.targetChainIds.length; // 1-based
        emit TargetAdded(chainId);
    }

    /// @inheritdoc IOriginRouter
    function removeTarget(uint32 chainId) external onlyRole(DEFAULT_ADMIN_ROLE) {
        OriginRouterStorage storage $ = _os();
        uint256 idxPlus1 = $.targetIndexPlus1[chainId];
        if (idxPlus1 == 0) revert TargetNotRegistered(chainId);
        uint32[] storage arr = $.targetChainIds;
        uint256 i = idxPlus1 - 1;
        uint256 last = arr.length - 1;
        if (i != last) {
            uint32 moved = arr[last];
            arr[i] = moved;
            $.targetIndexPlus1[moved] = i + 1; // keep the moved element 1-based
        }
        arr.pop();
        delete $.targetIndexPlus1[chainId];
        emit TargetRemoved(chainId);
    }

    /// @inheritdoc IOriginRouter
    function targets() external view returns (uint32[] memory) {
        return _os().targetChainIds;
    }

    /// @inheritdoc IOriginRouter
    function isTarget(uint32 chainId) external view returns (bool) {
        return _os().targetIndexPlus1[chainId] != 0;
    }

    /// @inheritdoc IOriginRouter
    function targetsOf(uint32 emissionDay) external view returns (uint32[] memory) {
        return _os().seriesTargets[emissionDay];
    }

    // --- Per-leg send isolation ---
    /// @dev External self-call seam so `try/catch` can isolate one leg. Only the contract itself
    ///      may call it.
    function sendLeg(uint32 dstChainId, bytes calldata payload, uint256 gasLimit) external returns (bytes32) {
        if (msg.sender != address(this)) revert OnlySelf();
        return _send(dstChainId, payload, gasLimit);
    }

    /// @dev Send one leg and emit its Sent event. On any failure, keep it for a permissionless retry and
    ///      continue. Returns 0 when kept.
    function _sendOrKeepFailed(uint32 dstChainId, bytes memory payload, uint256 gasLimit)
        private
        returns (bytes32 sendId)
    {
        try this.sendLeg(dstChainId, payload, gasLimit) returns (bytes32 id) {
            sendId = id;
            _emitSent(sendId, dstChainId, payload);
        } catch (bytes memory errorData) {
            _keepFailedMessageSend(dstChainId, payload, gasLimit, errorData);
        }
    }

    function _keepFailedMessageSend(uint32 dstChainId, bytes memory payload, uint256 gasLimit, bytes memory errorData)
        private
    {
        OriginRouterStorage storage $ = _os();
        uint256 idx = $.nextFailedMessageSendIdx++;
        FailedMessageSend storage p = $.failedMessageSends[idx];
        p.dstChainId = dstChainId;
        p.gasLimit = SafeCast.toUint64(gasLimit);
        p.payload = payload;
        // header layout: [version, msgType, ...]
        emit MessageSendFailed(idx, dstChainId, uint8(payload[1]), _keyOf(payload), errorData);
    }

    /// @inheritdoc IOriginRouter
    function retryMessageSend(uint256 idx) external nonReentrant {
        FailedMessageSend storage p = _os().failedMessageSends[idx];
        if (p.payload.length == 0 || p.sent) revert NoFailedMessageSend(idx);
        bytes memory payload = p.payload;
        if (uint8(payload[1]) == BridgeMsgCodec.MSG_INTEX_AUCTION_START) {
            uint32 emissionDay = _dayOf(payload);
            if (_os().intexAuctionCanceled[emissionDay]) revert IntexAuctionDayCanceled(emissionDay);
        }
        p.sent = true; // CEI. A revert in `_send` rolls this back and keeps the entry retryable.
        bytes32 sendId = _send(p.dstChainId, payload, p.gasLimit);
        _emitSent(sendId, p.dstChainId, payload);
    }

    /// @dev The Sent event of an auction message that left. Series and call notice sends emit their
    ///      own events at the call site.
    function _emitSent(bytes32 sendId, uint32 dstChainId, bytes memory payload) private {
        uint8 msgType = uint8(payload[1]);
        uint32 emissionDay = _dayOf(payload);
        if (msgType == BridgeMsgCodec.MSG_INTEX_AUCTION_START) {
            emit IntexAuctionStartSent(sendId, emissionDay, dstChainId);
        } else if (msgType == BridgeMsgCodec.MSG_INTEX_AUCTION_CLEARING_START) {
            emit IntexAuctionClearingStartSent(sendId, emissionDay, dstChainId);
        } else if (msgType == BridgeMsgCodec.MSG_INTEX_AUCTION_CANCEL) {
            emit IntexAuctionCancelSent(sendId, emissionDay, dstChainId);
        } else if (msgType == BridgeMsgCodec.MSG_INTEX_AUCTION_RESULT) {
            emit IntexAuctionResultSent(sendId, emissionDay, dstChainId, _word32(payload, 6), _word32(payload, 10));
        } else if (msgType == BridgeMsgCodec.MSG_INTEX_AUCTION_WINNERS) {
            uint256 winnersCount = _wordAt(payload, BridgeMsgCodec.HEADER_LEN + _bodyWord(payload, 5));
            emit IntexAuctionWinnersSent(
                sendId,
                emissionDay,
                dstChainId,
                uint16(_bodyWord(payload, 1)),
                uint16(_bodyWord(payload, 2)),
                // forge-lint: disable-next-line(unsafe-typecast) -- the codec caps a page at MAX_PAYLOAD_ARRAY_LEN
                uint32(winnersCount)
            );
        }
    }

    /// @dev The key of a kept message: its day, and for a paged message also its page.
    function _keyOf(bytes memory payload) private pure returns (bytes32) {
        uint8 msgType = uint8(payload[1]);
        uint256 day = _dayOf(payload);
        if (msgType == BridgeMsgCodec.MSG_INTEX_AUCTION_WINNERS || msgType == BridgeMsgCodec.MSG_INTEX_SERIES) {
            return bytes32((day << 16) | uint16(_bodyWord(payload, 1)));
        }
        return bytes32(day);
    }

    /// @dev The day of an outbound message: right after the header in a packed body, the first word of an
    ///      ABI-encoded one.
    function _dayOf(bytes memory payload) private pure returns (uint32) {
        uint8 msgType = uint8(payload[1]);
        if (
            msgType == BridgeMsgCodec.MSG_INTEX_AUCTION_WINNERS || msgType == BridgeMsgCodec.MSG_INTEX_SERIES
                || msgType == BridgeMsgCodec.MSG_INTEX_SERIES_CALL_NOTICE
        ) {
            return uint32(_bodyWord(payload, 0));
        }
        return _word32(payload, 2);
    }

    /// @dev The big-endian `uint32` at byte `at` of `payload`.
    function _word32(bytes memory payload, uint256 at) private pure returns (uint32) {
        return uint32(_wordAt(payload, at) >> 224);
    }

    /// @dev Word `i` of the ABI-encoded body after the two-byte header.
    function _bodyWord(bytes memory payload, uint256 i) private pure returns (uint256) {
        return _wordAt(payload, BridgeMsgCodec.HEADER_LEN + 32 * i);
    }

    /// @dev The 32 bytes at byte `at` of `payload`.
    function _wordAt(bytes memory payload, uint256 at) private pure returns (uint256 word) {
        assembly ("memory-safe") {
            word := mload(add(add(payload, 32), at))
        }
    }

    /// @inheritdoc IOriginRouter
    function getFailedMessageSend(uint256 idx) external view returns (FailedMessageSend memory) {
        return _os().failedMessageSends[idx];
    }

    /// @inheritdoc IOriginRouter
    function failedMessageSendCount() external view returns (uint256) {
        return _os().nextFailedMessageSendIdx;
    }

    /// @dev Whether `chainId` is in the series' STAGE_START snapshot (the frozen day-of target
    ///      set).
    function _isSeriesTarget(uint32 emissionDay, uint32 chainId) private view returns (bool) {
        uint32[] storage snapshot = _os().seriesTargets[emissionDay];
        uint256 len = snapshot.length;
        for (uint256 i = 0; i < len; ++i) {
            if (snapshot[i] == chainId) return true;
        }
        return false;
    }

    /// @dev Revert unless `dstChainId` is in the series' STAGE_START snapshot. Addressed sends
    ///      route only to a chain the day was actually started on (immune to a mid-day
    ///      removeTarget).
    function _requireSeriesTarget(uint32 emissionDay, uint32 dstChainId) private view {
        if (!_isSeriesTarget(emissionDay, dstChainId)) revert NotSeriesTarget(emissionDay, dstChainId);
    }

    /// @dev Reverts `InvalidDesisInterface(_desis)` if the target is an EOA or does not advertise
    ///      `IDesis` via ERC-165. Catches the common operator mistake of wiring a typo'd address
    ///      that would brick inbound.
    function _assertDesisInterface(address _desis) private view {
        if (_desis.code.length == 0) revert InvalidDesisInterface(_desis);
        try IERC165(_desis).supportsInterface(type(IDesis).interfaceId) returns (bool supported) {
            if (!supported) revert InvalidDesisInterface(_desis);
        } catch {
            revert InvalidDesisInterface(_desis);
        }
    }

    // --- Send ---
    /// @inheritdoc IOriginRouter
    function sendIntexAuctionStart(IntexAuctionStartParams calldata params) external payable onlyRole(DESIS_ROLE) {
        OriginRouterStorage storage $ = _os();
        uint32[] memory snapshot = $.targetChainIds;
        if (snapshot.length == 0) revert NoTargets();
        $.seriesTargets[params.emissionDay] = snapshot; // freeze the fan-out set for the whole day
        bytes memory payload = _encodeIntexAuctionStart(params);
        for (uint256 i = 0; i < snapshot.length; ++i) {
            _sendOrKeepFailed(snapshot[i], payload, IntexGas.intexAuctionStart(params.prices.length));
        }
    }

    /// @inheritdoc IOriginRouter
    function sendIntexAuctionCancel(uint32 emissionDay, uint8 reason) external payable onlyRole(DESIS_ROLE) {
        uint32[] memory snapshot = _os().seriesTargets[emissionDay];
        if (snapshot.length == 0) revert NoTargets();
        _os().intexAuctionCanceled[emissionDay] = true;
        bytes memory payload = BridgeMsgCodec.encodeIntexAuctionCancel(emissionDay, reason);
        for (uint256 i = 0; i < snapshot.length; ++i) {
            _sendOrKeepFailed(snapshot[i], payload, IntexGas.INTEX_AUCTION_CANCEL);
        }
    }

    /// @inheritdoc IOriginRouter
    function sendIntexAuctionClearingStart(uint32 emissionDay, uint32 dstChainId, uint256 gasLimit)
        external
        payable
        onlyRole(DESIS_ROLE)
    {
        if (!_isSeriesTarget(emissionDay, dstChainId)) revert NoTargets();
        uint64 budget = _clearingBudget(gasLimit);
        // Store the budget so the rounds that follow a remainder report ask for the same gas as the
        // first one.
        _os().clearingGas[emissionDay][dstChainId] = budget;
        _sendOrKeepFailed(dstChainId, BridgeMsgCodec.encodeIntexAuctionClearingStart(emissionDay), budget);
    }

    /// @dev Keep the ask inside what a round is worth. Below the floor, a round could not even flip
    ///      the stage. Above the cap, no target chain would accept the delivery.
    function _clearingBudget(uint256 gasLimit) private pure returns (uint64) {
        if (gasLimit < IntexGas.INTEX_AUCTION_CLEARING_START) return uint64(IntexGas.INTEX_AUCTION_CLEARING_START);
        if (gasLimit > IntexGas.INTEX_AUCTION_CLEARING_START_MAX) {
            return uint64(IntexGas.INTEX_AUCTION_CLEARING_START_MAX);
        }
        return uint64(gasLimit);
    }

    /// @inheritdoc IOriginRouter
    function sendIntexAuctionResult(
        uint32 dstChainId,
        uint32 emissionDay,
        uint32 wonQuantity,
        uint32 wonBidsCount,
        uint32 quantity,
        uint64 clearingRate,
        uint64 totalDemand
    ) external payable onlyRole(DESIS_ROLE) returns (bytes32 sendId) {
        _requireSeriesTarget(emissionDay, dstChainId);
        BridgeMsgCodec.IntexAuctionChainResult memory result = BridgeMsgCodec.IntexAuctionChainResult({
            wonQuantity: wonQuantity,
            wonBidsCount: wonBidsCount,
            quantity: quantity,
            clearingRate: clearingRate,
            totalDemand: totalDemand
        });
        sendId = _sendOrKeepFailed(
            dstChainId, BridgeMsgCodec.encodeIntexAuctionResult(emissionDay, result), IntexGas.INTEX_AUCTION_RESULT
        );
    }

    /// @inheritdoc IOriginRouter
    function sendIntexSeries(
        uint32 dstChainId,
        uint32 emissionDay,
        uint16 pageIndex,
        uint16 totalPages,
        IntexSeriesParams[] calldata series
    ) external payable onlyRole(INTEX_FACTORY_ROLE) returns (bytes32 sendId) {
        // The day must be one this chain is a target of. The codec owns the wire format (chunk
        // header, per-series day and counts).
        if (series.length == 0) revert EmptyArray();
        _requireSeriesTarget(emissionDay, dstChainId);
        sendId = _sendOrKeepFailed(
            dstChainId,
            BridgeMsgCodec.encodeIntexSeries(emissionDay, pageIndex, totalPages, _toCodecPayloads(series)),
            IntexGas.issuance(series.length)
        );
        emit IssuanceInstructionsSent(sendId, series[0].seriesId, series.length);
    }

    /// @inheritdoc IOriginRouter
    function sendIntexAuctionWinners(
        uint32 dstChainId,
        uint32 emissionDay,
        uint16 pageIndex,
        uint16 totalPages,
        uint64 clearingRate,
        uint128 basis,
        address[] calldata winners,
        uint16 partialIndex,
        uint16 partialWon
    ) external payable onlyRole(DESIS_ROLE) returns (bytes32 sendId) {
        _requireSeriesTarget(emissionDay, dstChainId);
        sendId = _sendOrKeepFailed(
            dstChainId,
            BridgeMsgCodec.encodeIntexAuctionWinners(
                emissionDay, pageIndex, totalPages, clearingRate, basis, winners, partialIndex, partialWon
            ),
            IntexGas.assetReturn(winners.length, _routesProceeds(winners.length))
        );
    }

    /// @dev Each chunk with winners routes its own proceeds, so it carries the leg.
    function _routesProceeds(uint256 winners) private pure returns (bool) {
        return winners != 0;
    }

    /// @inheritdoc IOriginRouter
    function sendIntexSeriesCallNotice(uint32 emissionDay, uint64 calledAt, bytes14[] calldata seriesIds)
        external
        payable
        onlyRole(INTEX_FACTORY_ROLE)
    {
        uint32[] memory snapshot = _os().seriesTargets[emissionDay];
        if (snapshot.length == 0) revert NoTargets();
        bytes memory payload = BridgeMsgCodec.encodeIntexSeriesCallNotice(emissionDay, calledAt, seriesIds);
        uint256 gasLimit = IntexGas.intexSeriesCallNotice(seriesIds.length);
        for (uint256 i = 0; i < snapshot.length; ++i) {
            bytes32 sendId = _sendOrKeepFailed(snapshot[i], payload, gasLimit);
            for (uint256 s = 0; s < seriesIds.length; ++s) {
                emit MarkCalledSent(sendId, seriesIds[s]);
            }
        }
    }

    // --- Receive ---
    /// @inheritdoc ERC7786MessengerBase
    /// @dev Guards the authenticated inbound path against re-entry through the Desis recipient.
    function receiveMessage(bytes32 receiveId, bytes calldata sender, bytes calldata payload)
        public
        payable
        override
        nonReentrant
        returns (bytes4)
    {
        return super.receiveMessage(receiveId, sender, payload);
    }

    /// @dev Decodes an authenticated inbound message and dispatches by msgType. BIDS_BATCH and
    ///      BIDS_TOTALS are inbound here. A premature message reverts, and the transport redelivers
    ///      it once its prerequisite has landed.
    function _dispatch(uint32 srcChainId, bytes32 receiveId, bytes calldata payload) internal override {
        uint8 msgType = BridgeMsgCodec.readHeader(payload);
        BridgeMsgCodec.assertMinLength(payload, msgType);

        if (msgType == BridgeMsgCodec.MSG_BIDS_PAGE) {
            _handleBidsPage(srcChainId, receiveId, payload);
        } else if (msgType == BridgeMsgCodec.MSG_BIDS_TOTALS) {
            _handleBidsTotals(srcChainId, receiveId, payload);
        } else if (msgType == BridgeMsgCodec.MSG_BIDS_PAUSE) {
            _handleBidsPause(srcChainId, receiveId, payload);
        } else {
            revert BridgeMsgCodec.UnknownMsgType(msgType);
        }
    }

    /// @dev Decode a BIDS_BATCH and forward it to Desis. The body `srcChainId` is cross-checked
    ///      against the authenticated source. Clearing does not fire here. The Desis begin-block
    ///      gate owns that.
    function _handleBidsPage(uint32 srcChainId, bytes32 receiveId, bytes calldata payload) internal {
        (
            uint32 emissionDay,
            uint32 bodySrcChainId,
            uint16 batchIndex,
            uint16 totalPages,
            address[] memory bidderAddresses,
            uint256[] memory packedBids
        ) = BridgeMsgCodec.decodeBidsPage(payload);

        if (!_acceptBids(receiveId, srcChainId, bodySrcChainId, emissionDay)) return;

        IDesis(_os().desis)
            .collectIntexAuctionBids(
                receiveId, emissionDay, srcChainId, batchIndex, totalPages, bidderAddresses, packedBids
            );
    }

    /// @dev Decode a BIDS_TOTALS marker and forward it to Desis. The body `srcChainId` is
    ///      cross-checked as in BIDS_BATCH.
    function _handleBidsTotals(uint32 srcChainId, bytes32 receiveId, bytes calldata payload) internal {
        (uint32 emissionDay, uint32 bodySrcChainId, uint16 totalPages, uint32 totalBids) =
            BridgeMsgCodec.decodeBidsTotals(payload);

        if (!_acceptBids(receiveId, srcChainId, bodySrcChainId, emissionDay)) return;

        IDesis(_os().desis).completeIntexAuctionBids(receiveId, emissionDay, srcChainId, totalPages, totalBids);
    }

    /// @dev A target whose relay stopped part way asks for another round. The router answers by
    ///      sending the day's CLEARING again. The target resumes from the chunk it left, so the
    ///      same message continues rather than starting over. The send is parked like any other
    ///      send when the relay float is empty.
    function _handleBidsPause(uint32 srcChainId, bytes32 receiveId, bytes calldata payload) private {
        (uint32 emissionDay, uint32 bodySrcChainId,,) = BridgeMsgCodec.decodeBidsPause(payload);
        if (!_acceptBids(receiveId, srcChainId, bodySrcChainId, emissionDay)) return;

        // A day whose intake has closed (cleared on the fan-in timeout, or canceled) would have
        // every chunk of the next round ignored on arrival. So the round is not worth paying for.
        IDesis.AuctionStage stage = IDesis(_os().desis).auctionStage(emissionDay);
        if (stage != IDesis.AuctionStage.RevealingBids && stage != IDesis.AuctionStage.CollectingBids) {
            emit MessageIgnored(receiveId, srcChainId, InboundReason.OBSOLETE);
            return;
        }

        uint64 budget = _os().clearingGas[emissionDay][srcChainId];
        uint256 gasLimit = budget == 0 ? IntexGas.INTEX_AUCTION_CLEARING_START : budget;
        bytes memory round = BridgeMsgCodec.encodeIntexAuctionClearingStart(emissionDay);
        _sendOrKeepFailed(srcChainId, round, gasLimit);
    }

    /// @dev Whether a relayed bids message may reach Desis. A body that names another source than
    ///      the one the bridge authenticated can never become acceptable. A source outside the
    ///      day's frozen snapshot can never become acceptable either. The router acknowledges such
    ///      a message without effect. Otherwise a rogue or late-registered source would leave
    ///      storage residue that Desis never clears, because Desis resets only snapshot chains.
    function _acceptBids(bytes32 receiveId, uint32 srcChainId, uint32 bodySrcChainId, uint32 emissionDay)
        private
        returns (bool)
    {
        if (bodySrcChainId != srcChainId) {
            emit MessageIgnored(receiveId, srcChainId, InboundReason.CONFLICT);
            return false;
        }
        if (!_isSeriesTarget(emissionDay, srcChainId)) {
            emit MessageIgnored(receiveId, srcChainId, InboundReason.NOT_FOUND);
            return false;
        }
        return true;
    }

    // --- Internal helpers ---
    /// @dev Encode an AUCTION_STAGE_START message from the grouped params struct.
    function _encodeIntexAuctionStart(IntexAuctionStartParams calldata p) private pure returns (bytes memory) {
        return BridgeMsgCodec.encodeIntexAuctionStart(
            p.emissionDay,
            p.commitEnd,
            p.revealEnd,
            p.issuanceEnd,
            p.promisLoadMinor,
            p.minBidRate,
            p.prices,
            p.callNoticePeriod,
            p.callWindow,
            p.callThreshold,
            p.minBidQuantity,
            p.bidBondMinor
        );
    }

    function _toCodecPayloads(IntexSeriesParams[] calldata series)
        private
        pure
        returns (BridgeMsgCodec.IntexSeriesPayload[] memory payloads)
    {
        payloads = new BridgeMsgCodec.IntexSeriesPayload[](series.length);
        for (uint256 i = 0; i < series.length; i++) {
            payloads[i] = _toCodecPayload(series[i]);
        }
    }

    function _toCodecPayload(IntexSeriesParams calldata p)
        private
        pure
        returns (BridgeMsgCodec.IntexSeriesPayload memory payload)
    {
        // Member-wise assignment (rather than a struct literal) keeps the payload within the IR
        // stack bound.
        payload.seriesId = p.seriesId;
        payload.emissionDay = p.emissionDay;
        payload.issuedAt = p.issuedAt;
        payload.issuedUnits = p.issuedUnits;
        payload.promisLoadMinor = p.promisLoadMinor;
        payload.entryPriceMinor = p.entryPriceMinor;
        payload.callNoticePeriod = p.callNoticePeriod;
        payload.issuanceCurrency = p.issuanceCurrency;
        payload.referenceCurrency = p.referenceCurrency;
        payload.callWindow = p.callWindow;
        payload.callThreshold = p.callThreshold;
        payload.callPriceMinor = p.callPriceMinor;
        payload.transitMargin = p.transitMargin;
    }

    /// @notice ERC-165 support check, resolving the AccessControl interface ids.
    function supportsInterface(bytes4 interfaceId) public view override(AccessControlUpgradeable) returns (bool) {
        return super.supportsInterface(interfaceId);
    }

    // --- Proceeds route ---
    /// @inheritdoc IOriginRouter
    function setProceedsRoute(address _tokenBridge, address _wcoen) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_tokenBridge == address(0)) revert ZeroAddress("tokenBridge");
        if (_wcoen == address(0)) revert ZeroAddress("wcoen");
        OriginRouterStorage storage $ = _os();
        $.tokenBridge = _tokenBridge;
        $.wcoen = _wcoen;
        emit OriginRouterProceedsRouteSet(_tokenBridge, _wcoen);
    }

    /// @inheritdoc IOriginRouter
    function tokenBridge() external view returns (address) {
        return _os().tokenBridge;
    }

    /// @inheritdoc IOriginRouter
    function wcoen() external view returns (address) {
        return _os().wcoen;
    }

    /// @inheritdoc IOriginRouter
    function getFailedIntexAuctionProceedsReceive(uint256 idx)
        external
        view
        returns (FailedIntexAuctionProceedsReceive memory)
    {
        return _os().failedIntexAuctionProceedsReceive[idx];
    }

    /// @inheritdoc IOriginRouter
    function failedIntexAuctionProceedsReceiveCount() external view returns (uint256) {
        return _os().nextFailedIntexAuctionProceedsReceiveIdx;
    }

    /// @inheritdoc IERC7786TokenReceiver
    /// @dev The token bridge credits WCOEN before this call. We unwrap it and hand the native to
    ///      the factory precompile, which burns it. A distribution failure parks
    ///      the native for retry, so the transfer still settles (returning the magic value) instead
    ///      of bricking on redelivery.
    function onCrosschainTokensReceived(
        uint32 sourceDomain,
        bytes calldata from,
        uint256 amount,
        bytes calldata extraData
    ) external nonReentrant returns (bytes4) {
        if (msg.sender != _os().tokenBridge) revert UnauthorizedProceedsCaller(msg.sender);
        (uint32 emissionDay, uint32 pageIndex) = abi.decode(extraData, (uint32, uint32));
        _checkProceedsSource(emissionDay, sourceDomain, from);
        _burnProceeds(emissionDay, pageIndex, sourceDomain, amount);
        return IERC7786TokenReceiver.onCrosschainTokensReceived.selector;
    }

    /// @inheritdoc IOriginRouter
    function receiveLocalProceeds(uint32 emissionDay, uint32 pageIndex, uint256 amount) external nonReentrant {
        uint32 local = uint32(block.chainid);
        _checkProceedsSource(emissionDay, local, InteroperableAddress.formatEvmV1(block.chainid, msg.sender));
        SafeERC20.safeTransferFrom(IERC20(_os().wcoen), msg.sender, address(this), amount);
        _burnProceeds(emissionDay, pageIndex, local, amount);
    }

    /// @dev The source must be in the day's frozen snapshot, and the sender must be its registered
    ///      TargetRouter. Otherwise anyone could open a distribution for any series.
    function _checkProceedsSource(uint32 emissionDay, uint32 sourceDomain, bytes memory from) private view {
        if (!_isSeriesTarget(emissionDay, sourceDomain)) revert UnexpectedProceedsSource(sourceDomain);
        if (keccak256(from) != keccak256(_remoteMessenger(sourceDomain))) revert UnauthorizedProceedsSender(from);
    }

    function _burnProceeds(uint32 emissionDay, uint32 pageIndex, uint32 sourceDomain, uint256 amount) private {
        IWCOEN(_os().wcoen).withdraw(amount);
        _burnOrKeepFailedIntexAuctionProceeds(emissionDay, pageIndex, sourceDomain, SafeCast.toUint128(amount));
    }

    /// @inheritdoc IOriginRouter
    function retryIntexAuctionProceedsReceive(uint256 idx) external nonReentrant {
        FailedIntexAuctionProceedsReceive storage p = _os().failedIntexAuctionProceedsReceive[idx];
        if (p.amount == 0 || p.settled) revert NoFailedIntexAuctionProceedsReceive(idx);
        p.settled = true;
        IIntexFactory(_os().intexFactory).burnIntexAuctionProceeds{value: p.amount}(p.emissionDay, p.srcChainId);
    }

    /// @dev Hand native proceeds to the factory precompile. On failure, keep them for a retry.
    function _burnOrKeepFailedIntexAuctionProceeds(
        uint32 emissionDay,
        uint32 pageIndex,
        uint32 srcChainId,
        uint128 amount
    ) private {
        // Both entries (onCrosschainTokensReceived, receiveLocalProceeds) are nonReentrant, so the
        // catch-branch write is safe.
        // slither-disable-next-line reentrancy-eth
        try IIntexFactory(_os().intexFactory).burnIntexAuctionProceeds{value: amount}(emissionDay, srcChainId) {}
        catch (bytes memory errorData) {
            OriginRouterStorage storage $ = _os();
            uint256 idx = $.nextFailedIntexAuctionProceedsReceiveIdx++;
            $.failedIntexAuctionProceedsReceive[idx] = FailedIntexAuctionProceedsReceive({
                emissionDay: emissionDay, srcChainId: srcChainId, amount: amount, settled: false
            });
            emit IntexAuctionProceedsReceiveFailed(idx, emissionDay, srcChainId, pageIndex, amount, errorData);
        }
    }
}
