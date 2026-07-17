// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";
import {SafeCast} from "@openzeppelin/contracts/utils/math/SafeCast.sol";

import {ERC7786MessengerBase} from "../shared/ERC7786MessengerBase.sol";
import {IOriginRouter} from "./interfaces/IOriginRouter.sol";
import {IDesis} from "./interfaces/IDesis.sol";
import {IIntexFactory} from "./interfaces/IIntexFactory.sol";
import {IWCOEN} from "./interfaces/IWCOEN.sol";
import {IERC7786TokenReceiver} from "./interfaces/IERC7786TokenReceiver.sol";
import {BridgeMsgCodec} from "../shared/libs/BridgeMsgCodec.sol";
import {IntexGas} from "../shared/libs/IntexGas.sol";

/// @title OriginRouter
/// @author Outbe
/// @notice Outbe-side router: sends auction/series messages to BNB and receives BIDS_BATCH from BNB over the
///         protocol-agnostic ERC-7786 bridge (the `crosschain` hub). The active transport is selected on the bridge.
/// @dev UUPS upgradeable behind an ERC1967 proxy; the bridge is an implementation immutable (from
///      {ERC7786MessengerBase}), so every upgrade must pass the same bridge to the constructor. All auction/series
///      auction messages are keyed by `worldwideDay`, series (issuance/mark) by `seriesId`.
contract OriginRouter is
    IOriginRouter,
    IERC7786TokenReceiver,
    ERC7786MessengerBase,
    AccessControlUpgradeable,
    ReentrancyGuardTransient,
    UUPSUpgradeable
{
    /// @notice Gates the demand-side sends: auction stages, AUCTION_RESULT, REFUND_INSTRUCTIONS.
    bytes32 public constant DESIS_ROLE = keccak256("DESIS_ROLE");
    /// @notice Gates the supply-side sends: ISSUANCE_INSTRUCTIONS, MARK_QUALIFIED, MARK_CALLED.
    bytes32 public constant INTEX_FACTORY_ROLE = keccak256("INTEX_FACTORY_ROLE");

    /// @notice Destination chainId of BNB — the sole peer for every outbound send and the only accepted source.
    uint32 public immutable BNB_CHAIN_ID;

    /// @custom:storage-location erc7201:outbe.intex.OriginRouter
    struct OriginRouterStorage {
        /// @dev Desis recipient that processes inbound BIDS_BATCH payloads (and holds `DESIS_ROLE`).
        address desis;
        /// @dev IntexFactory authorized for the supply-side sends (holds `INTEX_FACTORY_ROLE`).
        address intexFactory;
        /// @dev WCOEN token bridge authorized to invoke the proceeds hook.
        address tokenBridge;
        /// @dev WCOEN token unwrapped to native before distribution.
        address wcoen;
        /// @dev Parked distributions awaiting permissionless retry, by enqueue index.
        mapping(uint256 idx => ParkedProceeds) parkedProceeds;
        /// @dev Next index to assign in `parkedProceeds`.
        uint256 nextParkedProceedsIdx;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.OriginRouter")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0x2c9073d8b0cd30aa4aa3061d90da176c3d040651b0847724f0e9a4a76777f100;

    function _os() private pure returns (OriginRouterStorage storage $) {
        // solhint-disable-next-line no-inline-assembly
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address bridge_, uint32 bnbChainId_) ERC7786MessengerBase(bridge_) {
        BNB_CHAIN_ID = bnbChainId_;
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
    function wire(address _desis, address _intexFactory) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_desis == address(0)) revert ZeroAddress("desis");
        if (_intexFactory == address(0)) revert ZeroAddress("intexFactory");
        _assertDesisInterface(_desis);

        OriginRouterStorage storage $ = _os();
        address desisOld = $.desis;
        address intexFactoryOld = $.intexFactory;

        if ($.desis != address(0)) _revokeRole(DESIS_ROLE, $.desis);
        if ($.intexFactory != address(0)) _revokeRole(INTEX_FACTORY_ROLE, $.intexFactory);

        $.desis = _desis;
        $.intexFactory = _intexFactory;

        _grantRole(DESIS_ROLE, _desis);
        _grantRole(INTEX_FACTORY_ROLE, _intexFactory);

        emit DependenciesWired(desisOld, _desis, intexFactoryOld, _intexFactory);
    }

    /// @inheritdoc IOriginRouter
    function setRemoteMessenger(uint32 chainId, bytes calldata interop) external onlyRole(DEFAULT_ADMIN_ROLE) {
        _setRemoteMessenger(chainId, interop);
    }

    /// @dev Reverts `InvalidDesisInterface(_desis)` if the target is an EOA or does not advertise `IDesis` via
    ///      ERC-165. Catches the common operator mistake of wiring a typo'd address that would brick inbound.
    function _assertDesisInterface(address _desis) private view {
        if (_desis.code.length == 0) revert InvalidDesisInterface(_desis);
        try IERC165(_desis).supportsInterface(type(IDesis).interfaceId) returns (bool supported) {
            if (!supported) revert InvalidDesisInterface(_desis);
        } catch {
            revert InvalidDesisInterface(_desis);
        }
    }

    // --- Quote ---
    /// @inheritdoc IOriginRouter
    function quoteSendAuctionStageStart(AuctionStageStartParams calldata params) external view returns (uint256) {
        return _quoteFee(BNB_CHAIN_ID, _encodeAuctionStageStart(params), IntexGas.AUCTION_STAGE_START);
    }

    /// @inheritdoc IOriginRouter
    function quoteSendAuctionStageReveal(uint32 worldwideDay, bool isGreenDay) external view returns (uint256) {
        return _quoteFee(
            BNB_CHAIN_ID,
            BridgeMsgCodec.encodeAuctionStageReveal(worldwideDay, isGreenDay),
            IntexGas.AUCTION_STAGE_REVEAL
        );
    }

    /// @inheritdoc IOriginRouter
    function quoteSendAuctionStageClearing(uint32 worldwideDay) external view returns (uint256) {
        return _quoteFee(
            BNB_CHAIN_ID, BridgeMsgCodec.encodeAuctionStageClearing(worldwideDay), IntexGas.AUCTION_STAGE_CLEARING
        );
    }

    /// @inheritdoc IOriginRouter
    function quoteSendAuctionResult(
        uint32 worldwideDay,
        uint32 issuedIntexCount,
        uint64 auctionClearingRate,
        uint32 wonBidsCount
    ) external view returns (uint256) {
        return _quoteFee(
            BNB_CHAIN_ID,
            BridgeMsgCodec.encodeAuctionResult(worldwideDay, issuedIntexCount, auctionClearingRate, wonBidsCount),
            IntexGas.AUCTION_RESULT
        );
    }

    /// @inheritdoc IOriginRouter
    function quoteSendIssuanceInstructions(IssuanceInstructionsParams calldata params) external view returns (uint256) {
        return _quoteFee(
            BNB_CHAIN_ID,
            BridgeMsgCodec.encodeIssuanceInstructions(_toCodecPayload(params)),
            IntexGas.issuance(params.recipients.length)
        );
    }

    /// @inheritdoc IOriginRouter
    function quoteSendRefundInstructions(
        uint32 worldwideDay,
        address[] calldata bidders,
        uint128[] calldata refundedAmounts,
        uint128[] calldata paidAmounts
    ) external view returns (uint256) {
        return _quoteFee(
            BNB_CHAIN_ID,
            BridgeMsgCodec.encodeRefundInstructions(worldwideDay, bidders, refundedAmounts, paidAmounts),
            IntexGas.refund(bidders.length)
        );
    }

    /// @inheritdoc IOriginRouter
    function quoteSendMarkCalled(uint32 seriesId) external view returns (uint256) {
        return _quoteFee(BNB_CHAIN_ID, BridgeMsgCodec.encodeMarkCalled(seriesId), IntexGas.MARK_CALLED);
    }

    /// @inheritdoc IOriginRouter
    function quoteSendMarkQualified(uint32 seriesId) external view returns (uint256) {
        return _quoteFee(BNB_CHAIN_ID, BridgeMsgCodec.encodeMarkQualified(seriesId), IntexGas.MARK_QUALIFIED);
    }

    // --- Send ---
    /// @inheritdoc IOriginRouter
    function sendAuctionStageStart(AuctionStageStartParams calldata params)
        external
        payable
        onlyRole(DESIS_ROLE)
        returns (bytes32 sendId)
    {
        sendId = _send(BNB_CHAIN_ID, _encodeAuctionStageStart(params), IntexGas.AUCTION_STAGE_START);
        emit AuctionStageSent(sendId, params.worldwideDay, BridgeMsgCodec.MSG_AUCTION_STAGE_START);
    }

    /// @inheritdoc IOriginRouter
    function sendAuctionStageReveal(uint32 worldwideDay, bool isGreenDay)
        external
        payable
        onlyRole(DESIS_ROLE)
        returns (bytes32 sendId)
    {
        sendId = _send(
            BNB_CHAIN_ID,
            BridgeMsgCodec.encodeAuctionStageReveal(worldwideDay, isGreenDay),
            IntexGas.AUCTION_STAGE_REVEAL
        );
        emit AuctionStageSent(sendId, worldwideDay, BridgeMsgCodec.MSG_AUCTION_STAGE_REVEAL);
    }

    /// @inheritdoc IOriginRouter
    function sendAuctionStageClearing(uint32 worldwideDay)
        external
        payable
        onlyRole(DESIS_ROLE)
        returns (bytes32 sendId)
    {
        sendId = _send(
            BNB_CHAIN_ID, BridgeMsgCodec.encodeAuctionStageClearing(worldwideDay), IntexGas.AUCTION_STAGE_CLEARING
        );
        emit AuctionStageSent(sendId, worldwideDay, BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING);
    }

    /// @inheritdoc IOriginRouter
    function sendAuctionResult(
        uint32 worldwideDay,
        uint32 issuedIntexCount,
        uint64 auctionClearingRate,
        uint32 wonBidsCount
    ) external payable onlyRole(DESIS_ROLE) returns (bytes32 sendId) {
        sendId = _send(
            BNB_CHAIN_ID,
            BridgeMsgCodec.encodeAuctionResult(worldwideDay, issuedIntexCount, auctionClearingRate, wonBidsCount),
            IntexGas.AUCTION_RESULT
        );
        emit AuctionResultSent(sendId, worldwideDay, issuedIntexCount, auctionClearingRate);
    }

    /// @inheritdoc IOriginRouter
    function sendIssuanceInstructions(IssuanceInstructionsParams calldata params)
        external
        payable
        onlyRole(INTEX_FACTORY_ROLE)
        returns (bytes32 sendId)
    {
        uint256 len = params.recipients.length;
        if (len == 0) revert EmptyArray();
        if (len != params.quantities.length) revert ArrayLengthMismatch();

        sendId = _send(
            BNB_CHAIN_ID, BridgeMsgCodec.encodeIssuanceInstructions(_toCodecPayload(params)), IntexGas.issuance(len)
        );
        emit IssuanceInstructionsSent(sendId, params.seriesId, len);
    }

    /// @inheritdoc IOriginRouter
    function sendRefundInstructions(
        uint32 worldwideDay,
        address[] calldata bidders,
        uint128[] calldata refundedAmounts,
        uint128[] calldata paidAmounts
    ) external payable onlyRole(DESIS_ROLE) returns (bytes32 sendId) {
        uint256 len = bidders.length;
        if (len == 0) revert EmptyArray();
        if (len != refundedAmounts.length || len != paidAmounts.length) revert ArrayLengthMismatch();

        sendId = _send(
            BNB_CHAIN_ID,
            BridgeMsgCodec.encodeRefundInstructions(worldwideDay, bidders, refundedAmounts, paidAmounts),
            IntexGas.refund(len)
        );
        emit RefundInstructionsSent(sendId, worldwideDay, len);
    }

    /// @inheritdoc IOriginRouter
    function sendMarkCalled(uint32 seriesId) external payable onlyRole(INTEX_FACTORY_ROLE) returns (bytes32 sendId) {
        sendId = _send(BNB_CHAIN_ID, BridgeMsgCodec.encodeMarkCalled(seriesId), IntexGas.MARK_CALLED);
        emit MarkCalledSent(sendId, seriesId);
    }

    /// @inheritdoc IOriginRouter
    function sendMarkQualified(uint32 seriesId) external payable onlyRole(INTEX_FACTORY_ROLE) returns (bytes32 sendId) {
        sendId = _send(BNB_CHAIN_ID, BridgeMsgCodec.encodeMarkQualified(seriesId), IntexGas.MARK_QUALIFIED);
        emit MarkQualifiedSent(sendId, seriesId);
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

    /// @dev Decodes an authenticated inbound message and dispatches by msgType. Only BIDS_BATCH is inbound here; a
    ///      premature message reverts and is redelivered by the transport once its prerequisite has landed.
    function _dispatch(
        uint32 srcChainId,
        bytes32,
        /*receiveId*/
        bytes calldata payload
    )
        internal
        override
    {
        uint8 msgType = BridgeMsgCodec.readHeader(payload);
        BridgeMsgCodec.assertMinLength(payload, msgType);

        if (msgType == BridgeMsgCodec.MSG_BIDS_BATCH) {
            _handleBidsBatch(srcChainId, payload);
        } else {
            revert BridgeMsgCodec.UnknownMsgType(msgType);
        }
    }

    /// @dev Decode a BIDS_BATCH, forward it to Desis, and auto-fire clearing when the series is ready. The body
    ///      carries its own `srcChainId`; it is cross-checked against the authenticated source. Desis tracks
    ///      per-(series, generation) completeness from `batchIndex`/`totalBatches`, so batches of one flush may
    ///      arrive in any order over the unordered bridge. A clearing-side revert is caught locally and surfaced
    ///      without rolling back the bid intake.
    function _handleBidsBatch(uint32 srcChainId, bytes calldata payload) internal {
        (
            uint32 worldwideDay,
            uint32 bodySrcChainId,
            uint32 relayGeneration,
            uint16 batchIndex,
            uint16 totalBatches,
            address[] memory bidderAddresses,
            uint16[] memory intexQuantities,
            uint32[] memory intexBidRates,
            uint32[] memory timestamps
        ) = BridgeMsgCodec.decodeBidsBatch(payload);

        if (bodySrcChainId != srcChainId) revert SrcChainIdBodyMismatch(srcChainId, bodySrcChainId);

        address desisRecipient = _os().desis;
        IDesis(desisRecipient)
            .processBidsBatch(
                worldwideDay,
                srcChainId,
                relayGeneration,
                batchIndex,
                totalBatches,
                bidderAddresses,
                intexQuantities,
                intexBidRates,
                timestamps
            );

        emit BidsBatchReceived(srcChainId, worldwideDay, bidderAddresses.length);

        // Auto-fire clearing once bids are committed. Local try/catch so a clearing-side revert does not roll back
        // the bid intake — operators can retry clearAuction manually if needed.
        if (IDesis(desisRecipient).getAuctionStage(worldwideDay) == IDesis.AuctionStage.BidsReceived) {
            try IDesis(desisRecipient).clearAuction(worldwideDay) {
                emit ClearingAutoDispatched(worldwideDay);
            } catch (bytes memory reason) {
                emit ClearingAutoDispatchFailed(worldwideDay, reason);
            }
        }
    }

    // --- Internal helpers ---
    /// @dev Encode an AUCTION_STAGE_START message from the grouped params struct.
    function _encodeAuctionStageStart(AuctionStageStartParams calldata p) private pure returns (bytes memory) {
        return BridgeMsgCodec.encodeAuctionStageStart(
            p.worldwideDay,
            p.commitEnd,
            p.revealEnd,
            p.issuanceEnd,
            p.issuanceCurrency,
            p.referenceCurrency,
            p.promisLoadMinor,
            p.minIntexBidRate,
            p.entryPrice,
            p.floorPriceMinor,
            p.callPriceMinor,
            p.intexCallPeriod,
            p.callWindowDays,
            p.callThresholdDays,
            p.minIntexBidQuantity,
            p.commitBondMinor
        );
    }

    function _toCodecPayload(IssuanceInstructionsParams calldata p)
        private
        pure
        returns (BridgeMsgCodec.IssuanceInstructionsPayload memory payload)
    {
        // Member-wise assignment (rather than a struct literal) keeps the 14-field payload within the IR stack bound.
        payload.seriesId = p.seriesId;
        payload.issuedIntexCount = p.issuedIntexCount;
        payload.promisLoadMinor = p.promisLoadMinor;
        payload.entryPriceMinor = p.entryPriceMinor;
        payload.floorPriceMinor = p.floorPriceMinor;
        payload.intexCallPeriod = p.intexCallPeriod;
        payload.issuanceCurrency = p.issuanceCurrency;
        payload.referenceCurrency = p.referenceCurrency;
        payload.callWindowDays = p.callWindowDays;
        payload.callThresholdDays = p.callThresholdDays;
        payload.callPriceMinor = p.callPriceMinor;
        payload.recipients = p.recipients;
        payload.quantities = p.quantities;
    }

    /// @notice ERC-165 support check, resolving the AccessControl interface ids.
    function supportsInterface(bytes4 interfaceId) public view override(AccessControlUpgradeable) returns (bool) {
        return super.supportsInterface(interfaceId);
    }

    /// @inheritdoc IOriginRouter
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

    // --- Proceeds route ---
    /// @inheritdoc IOriginRouter
    function setProceedsRoute(address _tokenBridge, address _wcoen) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_tokenBridge == address(0)) revert ZeroAddress("tokenBridge");
        if (_wcoen == address(0)) revert ZeroAddress("wcoen");
        OriginRouterStorage storage $ = _os();
        $.tokenBridge = _tokenBridge;
        $.wcoen = _wcoen;
        emit ProceedsRouteSet(_tokenBridge, _wcoen);
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
    function parkedProceeds(uint256 idx) external view returns (ParkedProceeds memory) {
        return _os().parkedProceeds[idx];
    }

    /// @inheritdoc IERC7786TokenReceiver
    /// @dev The token bridge credits WCOEN before this call; we unwrap it and hand the native to the factory
    ///      precompile, which pays the series' creators. A distribution failure parks the native for retry so
    ///      the transfer still settles (returning the magic value) instead of bricking on redelivery.
    function onCrosschainTokensReceived(
        uint32 sourceDomain,
        bytes calldata from,
        uint256 amount,
        bytes calldata extraData
    ) external nonReentrant returns (bytes4) {
        OriginRouterStorage storage $ = _os();
        if (msg.sender != $.tokenBridge) revert UnauthorizedProceedsCaller(msg.sender);
        if (sourceDomain != BNB_CHAIN_ID) revert UnexpectedProceedsSource(sourceDomain);
        // The bridge is permissionless: pin the source sender to the registered BNB peer (TargetRouter), else
        // anyone could open a distribution for any series and wipe its contributor provenance.
        if (keccak256(from) != keccak256(_remoteMessenger(sourceDomain))) revert UnauthorizedProceedsSender(from);

        uint32 worldwideDay = abi.decode(extraData, (uint32));
        IWCOEN($.wcoen).withdraw(amount);
        _distributeOrPark(worldwideDay, SafeCast.toUint128(amount));

        return IERC7786TokenReceiver.onCrosschainTokensReceived.selector;
    }

    /// @inheritdoc IOriginRouter
    function retryProceeds(uint256 idx) external nonReentrant {
        ParkedProceeds storage p = _os().parkedProceeds[idx];
        if (p.amount == 0 || p.settled) revert NoParkedProceeds(idx);
        p.settled = true;
        IIntexFactory(_os().intexFactory).distribute{value: p.amount}(p.worldwideDay);
        emit ProceedsRetried(idx, p.worldwideDay, p.amount);
    }

    /// @dev Hand native proceeds to the factory precompile; park them for retry on failure.
    function _distributeOrPark(uint32 worldwideDay, uint128 amount) private {
        // The sole caller (onCrosschainTokensReceived) is nonReentrant, so the catch-branch park write is safe.
        // slither-disable-next-line reentrancy-eth
        try IIntexFactory(_os().intexFactory).distribute{value: amount}(worldwideDay) {
            emit ProceedsDistributed(worldwideDay, amount);
        } catch {
            OriginRouterStorage storage $ = _os();
            uint256 idx = $.nextParkedProceedsIdx++;
            $.parkedProceeds[idx] = ParkedProceeds({worldwideDay: worldwideDay, amount: amount, settled: false});
            emit ProceedsParked(idx, worldwideDay, amount);
        }
    }
}
