// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";

import {ERC7786MessengerBase} from "../shared/ERC7786MessengerBase.sol";
import {IOriginMessenger} from "./interfaces/IOriginMessenger.sol";
import {IDesis} from "./interfaces/IDesis.sol";
import {BridgeMsgCodec} from "../shared/libs/BridgeMsgCodec.sol";
import {IntexGas} from "../shared/libs/IntexGas.sol";

/// @title OriginMessenger
/// @author Outbe
/// @notice Outbe-side messenger: sends auction/series messages to BNB and receives BIDS_BATCH from BNB over the
///         protocol-agnostic ERC-7786 bridge (the `crosschain` hub). The active transport is selected on the bridge.
/// @dev UUPS upgradeable behind an ERC1967 proxy; the bridge is an implementation immutable (from
///      {ERC7786MessengerBase}), so every upgrade must pass the same bridge to the constructor. All auction/series
///      messages are keyed by `seriesId` (uint32).
contract OriginMessenger is
    IOriginMessenger,
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

    /// @custom:storage-location erc7201:outbe.intex.OriginMessenger
    struct OriginMessengerStorage {
        /// @dev Desis recipient that processes inbound BIDS_BATCH payloads (and holds `DESIS_ROLE`).
        address desis;
        /// @dev IntexFactory authorized for the supply-side sends (holds `INTEX_FACTORY_ROLE`).
        address intexFactory;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.OriginMessenger")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0xb52fe309ffa163cf95d112f1416b87c89560d9d1ded95f235e82ca4df07af800;

    function _os() private pure returns (OriginMessengerStorage storage $) {
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
    /// @inheritdoc IOriginMessenger
    function wire(address _desis, address _intexFactory) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_desis == address(0)) revert ZeroAddress("desis");
        if (_intexFactory == address(0)) revert ZeroAddress("intexFactory");
        _assertDesisInterface(_desis);

        OriginMessengerStorage storage $ = _os();
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

    /// @inheritdoc IOriginMessenger
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
    /// @inheritdoc IOriginMessenger
    function quoteSendAuctionStageStart(AuctionStageStartParams calldata params) external view returns (uint256) {
        return _quoteFee(BNB_CHAIN_ID, _encodeAuctionStageStart(params), IntexGas.AUCTION_STAGE_START);
    }

    /// @inheritdoc IOriginMessenger
    function quoteSendAuctionStageReveal(uint32 seriesId, bool isGreenDay) external view returns (uint256) {
        return _quoteFee(
            BNB_CHAIN_ID, BridgeMsgCodec.encodeAuctionStageReveal(seriesId, isGreenDay), IntexGas.AUCTION_STAGE_REVEAL
        );
    }

    /// @inheritdoc IOriginMessenger
    function quoteSendAuctionStageClearing(uint32 seriesId) external view returns (uint256) {
        return
            _quoteFee(
                BNB_CHAIN_ID, BridgeMsgCodec.encodeAuctionStageClearing(seriesId), IntexGas.AUCTION_STAGE_CLEARING
            );
    }

    /// @inheritdoc IOriginMessenger
    function quoteSendAuctionResult(
        uint32 seriesId,
        uint32 issuedIntexCount,
        uint64 auctionClearingRate,
        uint32 wonBidsCount
    ) external view returns (uint256) {
        return _quoteFee(
            BNB_CHAIN_ID,
            BridgeMsgCodec.encodeAuctionResult(seriesId, issuedIntexCount, auctionClearingRate, wonBidsCount),
            IntexGas.AUCTION_RESULT
        );
    }

    /// @inheritdoc IOriginMessenger
    function quoteSendIssuanceInstructions(IssuanceInstructionsParams calldata params) external view returns (uint256) {
        return _quoteFee(
            BNB_CHAIN_ID,
            BridgeMsgCodec.encodeIssuanceInstructions(_toCodecPayload(params)),
            IntexGas.issuance(params.recipients.length)
        );
    }

    /// @inheritdoc IOriginMessenger
    function quoteSendRefundInstructions(
        uint32 seriesId,
        address[] calldata bidders,
        uint128[] calldata refundedAmounts,
        uint128[] calldata paidAmounts
    ) external view returns (uint256) {
        return _quoteFee(
            BNB_CHAIN_ID,
            BridgeMsgCodec.encodeRefundInstructions(seriesId, bidders, refundedAmounts, paidAmounts),
            IntexGas.refund(bidders.length)
        );
    }

    /// @inheritdoc IOriginMessenger
    function quoteSendMarkCalled(uint32 seriesId) external view returns (uint256) {
        return _quoteFee(BNB_CHAIN_ID, BridgeMsgCodec.encodeMarkCalled(seriesId), IntexGas.MARK_CALLED);
    }

    /// @inheritdoc IOriginMessenger
    function quoteSendMarkQualified(uint32 seriesId) external view returns (uint256) {
        return _quoteFee(BNB_CHAIN_ID, BridgeMsgCodec.encodeMarkQualified(seriesId), IntexGas.MARK_QUALIFIED);
    }

    // --- Send ---
    /// @inheritdoc IOriginMessenger
    function sendAuctionStageStart(AuctionStageStartParams calldata params)
        external
        payable
        onlyRole(DESIS_ROLE)
        returns (bytes32 sendId)
    {
        sendId = _send(BNB_CHAIN_ID, _encodeAuctionStageStart(params), IntexGas.AUCTION_STAGE_START);
        emit AuctionStageSent(sendId, params.seriesId, BridgeMsgCodec.MSG_AUCTION_STAGE_START);
    }

    /// @inheritdoc IOriginMessenger
    function sendAuctionStageReveal(uint32 seriesId, bool isGreenDay)
        external
        payable
        onlyRole(DESIS_ROLE)
        returns (bytes32 sendId)
    {
        sendId = _send(
            BNB_CHAIN_ID, BridgeMsgCodec.encodeAuctionStageReveal(seriesId, isGreenDay), IntexGas.AUCTION_STAGE_REVEAL
        );
        emit AuctionStageSent(sendId, seriesId, BridgeMsgCodec.MSG_AUCTION_STAGE_REVEAL);
    }

    /// @inheritdoc IOriginMessenger
    function sendAuctionStageClearing(uint32 seriesId) external payable onlyRole(DESIS_ROLE) returns (bytes32 sendId) {
        sendId =
            _send(BNB_CHAIN_ID, BridgeMsgCodec.encodeAuctionStageClearing(seriesId), IntexGas.AUCTION_STAGE_CLEARING);
        emit AuctionStageSent(sendId, seriesId, BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING);
    }

    /// @inheritdoc IOriginMessenger
    function sendAuctionResult(
        uint32 seriesId,
        uint32 issuedIntexCount,
        uint64 auctionClearingRate,
        uint32 wonBidsCount
    ) external payable onlyRole(DESIS_ROLE) returns (bytes32 sendId) {
        sendId = _send(
            BNB_CHAIN_ID,
            BridgeMsgCodec.encodeAuctionResult(seriesId, issuedIntexCount, auctionClearingRate, wonBidsCount),
            IntexGas.AUCTION_RESULT
        );
        emit AuctionResultSent(sendId, seriesId, issuedIntexCount, auctionClearingRate);
    }

    /// @inheritdoc IOriginMessenger
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

    /// @inheritdoc IOriginMessenger
    function sendRefundInstructions(
        uint32 seriesId,
        address[] calldata bidders,
        uint128[] calldata refundedAmounts,
        uint128[] calldata paidAmounts
    ) external payable onlyRole(DESIS_ROLE) returns (bytes32 sendId) {
        uint256 len = bidders.length;
        if (len == 0) revert EmptyArray();
        if (len != refundedAmounts.length || len != paidAmounts.length) revert ArrayLengthMismatch();

        sendId = _send(
            BNB_CHAIN_ID,
            BridgeMsgCodec.encodeRefundInstructions(seriesId, bidders, refundedAmounts, paidAmounts),
            IntexGas.refund(len)
        );
        emit RefundInstructionsSent(sendId, seriesId, len);
    }

    /// @inheritdoc IOriginMessenger
    function sendMarkCalled(uint32 seriesId) external payable onlyRole(INTEX_FACTORY_ROLE) returns (bytes32 sendId) {
        sendId = _send(BNB_CHAIN_ID, BridgeMsgCodec.encodeMarkCalled(seriesId), IntexGas.MARK_CALLED);
        emit MarkCalledSent(sendId, seriesId);
    }

    /// @inheritdoc IOriginMessenger
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
            uint32 seriesId,
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
                seriesId,
                srcChainId,
                relayGeneration,
                batchIndex,
                totalBatches,
                bidderAddresses,
                intexQuantities,
                intexBidRates,
                timestamps
            );

        emit BidsBatchReceived(srcChainId, seriesId, bidderAddresses.length);

        // Auto-fire clearing once bids are committed. Local try/catch so a clearing-side revert does not roll back
        // the bid intake — operators can retry clearAuction manually if needed.
        if (IDesis(desisRecipient).getAuctionStage(seriesId) == IDesis.AuctionStage.BidsReceived) {
            try IDesis(desisRecipient).clearAuction(seriesId) {
                emit ClearingAutoDispatched(seriesId);
            } catch (bytes memory reason) {
                emit ClearingAutoDispatchFailed(seriesId, reason);
            }
        }
    }

    // --- Internal helpers ---
    /// @dev Encode an AUCTION_STAGE_START message from the grouped params struct.
    function _encodeAuctionStageStart(AuctionStageStartParams calldata p) private pure returns (bytes memory) {
        return BridgeMsgCodec.encodeAuctionStageStart(
            p.seriesId,
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
            p.minIntexBidQuantity
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

    /// @inheritdoc IOriginMessenger
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
}
