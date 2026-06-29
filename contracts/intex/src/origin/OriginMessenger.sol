// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {ReentrancyGuardUpgradeable} from "@openzeppelin/contracts-upgradeable/utils/ReentrancyGuardUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";
import {
    OAppUpgradeable,
    Origin,
    MessagingFee,
    MessagingReceipt
} from "@layerzerolabs/oapp-evm-upgradeable/oapp/OAppUpgradeable.sol";
import {
    OAppOptionsType3Upgradeable
} from "@layerzerolabs/oapp-evm-upgradeable/oapp/libs/OAppOptionsType3Upgradeable.sol";

import {IOriginMessenger} from "./interfaces/IOriginMessenger.sol";
import {IDesis} from "./interfaces/IDesis.sol";
import {BridgeMsgCodec} from "../shared/libs/BridgeMsgCodec.sol";
import {LzGasEstimator} from "../shared/libs/LzGasEstimator.sol";

/// @title OriginMessenger
/// @author Outbe
/// @notice LayerZero bridge adapter for Outbe Chain.
/// @dev UUPS upgradeable: deployed behind an ERC1967 proxy; the LayerZero endpoint stays an
///      implementation immutable, so every upgrade must pass the same endpoint to the constructor.
///      Sends messages to BNB, receives messages from BNB. All auction/series
///      messages are keyed by `seriesId` (uint32).
contract OriginMessenger is
    IOriginMessenger,
    OAppUpgradeable,
    OAppOptionsType3Upgradeable,
    AccessControlUpgradeable,
    ReentrancyGuardUpgradeable,
    UUPSUpgradeable
{
    /// @notice Gates the demand-side sends: auction stages, AUCTION_RESULT, REFUND_INSTRUCTIONS.
    bytes32 public constant DESIS_ROLE = keccak256("DESIS_ROLE");
    /// @notice Gates the supply-side sends: ISSUANCE_INSTRUCTIONS, MARK_QUALIFIED, MARK_CALLED.
    bytes32 public constant INTEX_FACTORY_ROLE = keccak256("INTEX_FACTORY_ROLE");

    /// @notice Destination gas for inbound ISSUANCE_INSTRUCTIONS: createSeries + per-recipient mintBatch.
    /// @dev Calibrated via GasCalibration.t.sol; LzGasEstimator adds +20%.
    uint128 internal constant ISSUANCE_BASE_GAS = 300_000;
    uint128 internal constant ISSUANCE_PER_ITEM_GAS = 230_000;

    /// @notice Destination gas for inbound REFUND_INSTRUCTIONS: decode + per-bidder finalizeAuction.
    /// @dev Calibrated via GasCalibration.t.sol (Compact mocked, so per-item is a lower bound + headroom).
    uint128 internal constant REFUND_BASE_GAS = 250_000;
    uint128 internal constant REFUND_PER_ITEM_GAS = 160_000;

    /// @notice LayerZero endpoint id of the BNB chain — the sole peer for every outbound send and the
    ///         only accepted inbound source.
    uint32 public immutable BNB_EID;

    /// @custom:storage-location erc7201:outbe.intex.OriginMessenger
    struct OriginMessengerStorage {
        /// @dev Desis recipient that processes inbound BIDS_BATCH payloads (and holds `DESIS_ROLE`).
        address desis;
        /// @dev IntexFactory authorized for the supply-side sends (holds `INTEX_FACTORY_ROLE`).
        address intexFactory;
        /// @dev Last inbound LayerZero nonce successfully processed for each `(srcEid, sender)` pair.
        ///      Backs the `nextNonce` override that switches this OApp into ORDERED-delivery mode.
        mapping(uint32 srcEid => mapping(bytes32 sender => uint64 nonce)) inboundNonce;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.OriginMessenger")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0xb52fe309ffa163cf95d112f1416b87c89560d9d1ded95f235e82ca4df07af800;

    function _s() private pure returns (OriginMessengerStorage storage $) {
        // solhint-disable-next-line no-inline-assembly
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address _lzEndpoint, uint32 _bnbEid) OAppUpgradeable(_lzEndpoint) {
        BNB_EID = _bnbEid;
        _disableInitializers();
    }

    /// @notice Initializes the proxy: LayerZero delegate, contract owner, and admin role holder.
    /// @param _delegate Owner, endpoint delegate, and receiver of `DEFAULT_ADMIN_ROLE`.
    function initialize(address _delegate) external initializer {
        if (_delegate == address(0)) revert ZeroAddress("delegate");

        __Ownable_init(_delegate);
        __OApp_init(_delegate);
        __AccessControl_init();
        __ReentrancyGuard_init();
        __UUPSUpgradeable_init();

        _grantRole(DEFAULT_ADMIN_ROLE, _delegate);
    }

    /// @dev Upgrades are gated by the admin role.
    /// @param newImplementation Address of the implementation the proxy switches to.
    // solhint-disable-next-line no-empty-blocks
    function _authorizeUpgrade(address newImplementation) internal override onlyRole(DEFAULT_ADMIN_ROLE) {}

    // --- Storage getters ---
    /// @notice Desis recipient that processes inbound BIDS_BATCH payloads (and holds `DESIS_ROLE`).
    /// @return The wired Desis address.
    function desis() external view returns (address) {
        return _s().desis;
    }

    /// @notice IntexFactory authorized for the supply-side sends (holds `INTEX_FACTORY_ROLE`).
    /// @return The wired IntexFactory address.
    function intexFactory() external view returns (address) {
        return _s().intexFactory;
    }

    /// @notice Last inbound LayerZero nonce successfully processed for a `(srcEid, sender)` pair.
    /// @param srcEid LayerZero source endpoint id of the channel.
    /// @param sender Bytes32-encoded peer address on the source chain.
    /// @return The last processed inbound nonce.
    function inboundNonce(uint32 srcEid, bytes32 sender) external view returns (uint64) {
        return _s().inboundNonce[srcEid][sender];
    }

    // --- Admin ---
    /// @inheritdoc IOriginMessenger
    function wire(address _desis, address _intexFactory) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_desis == address(0)) revert ZeroAddress("desis");
        if (_intexFactory == address(0)) revert ZeroAddress("intexFactory");
        _assertDesisInterface(_desis);

        OriginMessengerStorage storage $ = _s();
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

    /// @dev Reverts `InvalidDesisInterface(_desis)` if the target is an EOA or does not advertise
    ///      `IDesis` via ERC-165. Catches the common operator mistake of wiring a typo'd address
    ///      that would otherwise silently bind and brick every inbound BIDS_BATCH.
    function _assertDesisInterface(address _desis) private view {
        if (_desis.code.length == 0) revert InvalidDesisInterface(_desis);
        try IERC165(_desis).supportsInterface(type(IDesis).interfaceId) returns (bool supported) {
            if (!supported) revert InvalidDesisInterface(_desis);
        } catch {
            revert InvalidDesisInterface(_desis);
        }
    }

    // --- Quote Functions ---
    /// @inheritdoc IOriginMessenger
    function quoteSendAuctionStageStart(
        AuctionStageStartParams calldata params,
        bytes calldata extraOptions,
        bool payInLzToken
    ) external view returns (MessagingFee memory fee) {
        bytes memory message = _encodeAuctionStageStart(params);
        bytes memory options = combineOptions(BNB_EID, BridgeMsgCodec.MSG_AUCTION_STAGE_START, extraOptions);
        return _quote(BNB_EID, message, options, payInLzToken);
    }

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

    /// @inheritdoc IOriginMessenger
    function quoteSendAuctionStageReveal(
        uint32 seriesId,
        bool isGreenDay,
        bytes calldata extraOptions,
        bool payInLzToken
    ) external view returns (MessagingFee memory fee) {
        bytes memory message = BridgeMsgCodec.encodeAuctionStageReveal(seriesId, isGreenDay);
        bytes memory options = combineOptions(BNB_EID, BridgeMsgCodec.MSG_AUCTION_STAGE_REVEAL, extraOptions);
        return _quote(BNB_EID, message, options, payInLzToken);
    }

    /// @inheritdoc IOriginMessenger
    function quoteSendAuctionStageClearing(uint32 seriesId, bytes calldata extraOptions, bool payInLzToken)
        external
        view
        returns (MessagingFee memory fee)
    {
        bytes memory message = BridgeMsgCodec.encodeAuctionStageClearing(seriesId);
        bytes memory options = combineOptions(BNB_EID, BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING, extraOptions);
        return _quote(BNB_EID, message, options, payInLzToken);
    }

    /// @inheritdoc IOriginMessenger
    function quoteSendAuctionResult(
        uint32 seriesId,
        uint32 issuedIntexCount,
        uint64 auctionClearingRate,
        uint32 wonBidsCount,
        bytes calldata extraOptions,
        bool payInLzToken
    ) external view returns (MessagingFee memory fee) {
        bytes memory message = BridgeMsgCodec.encodeAuctionResult(
            seriesId, issuedIntexCount, auctionClearingRate, wonBidsCount
        );
        bytes memory options = combineOptions(BNB_EID, BridgeMsgCodec.MSG_AUCTION_RESULT, extraOptions);
        return _quote(BNB_EID, message, options, payInLzToken);
    }

    /// @inheritdoc IOriginMessenger
    function quoteSendIssuanceInstructions(
        IssuanceInstructionsParams calldata params,
        bytes calldata extraOptions,
        bool payInLzToken
    ) external view returns (MessagingFee memory fee) {
        return _quote(
            BNB_EID,
            BridgeMsgCodec.encodeIssuanceInstructions(_toCodecPayload(params)),
            _issuanceReceiveOption(params.recipients.length),
            payInLzToken
        );
    }

    /// @dev Destination `lzReceiveOption` sized for an inbound ISSUANCE_INSTRUCTIONS of
    ///      `recipientCount` recipients (`intex.mintBatch` loop) —.
    function _issuanceReceiveOption(uint256 recipientCount) internal pure returns (bytes memory) {
        return LzGasEstimator.receiveOption(ISSUANCE_BASE_GAS, ISSUANCE_PER_ITEM_GAS, recipientCount);
    }

    /// @dev Destination `lzReceiveOption` sized for an inbound REFUND_INSTRUCTIONS of `bidderCount`
    ///      bidders (`escrowAdapter.finalizeAuction` per-bidder loop) —.
    function _refundReceiveOption(uint256 bidderCount) internal pure returns (bytes memory) {
        return LzGasEstimator.receiveOption(REFUND_BASE_GAS, REFUND_PER_ITEM_GAS, bidderCount);
    }

    function _toCodecPayload(IssuanceInstructionsParams calldata p)
        private
        pure
        returns (BridgeMsgCodec.IssuanceInstructionsPayload memory payload)
    {
        // Member-wise assignment (rather than a single struct literal) keeps the 14-field payload
        // within the IR stack bound under via_ir.
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

    /// @inheritdoc IOriginMessenger
    function quoteSendRefundInstructions(
        uint32 seriesId,
        address[] calldata bidders,
        uint128[] calldata refundedAmounts,
        uint128[] calldata paidAmounts,
        bytes calldata extraOptions,
        bool payInLzToken
    ) external view returns (MessagingFee memory fee) {
        bytes memory message = BridgeMsgCodec.encodeRefundInstructions(seriesId, bidders, refundedAmounts, paidAmounts);
        bytes memory options = _refundReceiveOption(bidders.length);
        return _quote(BNB_EID, message, options, payInLzToken);
    }

    /// @inheritdoc IOriginMessenger
    function quoteSendMarkCalled(uint32 seriesId, bytes calldata extraOptions, bool payInLzToken)
        external
        view
        returns (MessagingFee memory fee)
    {
        bytes memory message = BridgeMsgCodec.encodeMarkCalled(seriesId);
        bytes memory options = combineOptions(BNB_EID, BridgeMsgCodec.MSG_MARK_CALLED, extraOptions);
        return _quote(BNB_EID, message, options, payInLzToken);
    }

    /// @inheritdoc IOriginMessenger
    function quoteSendMarkQualified(uint32 seriesId, bytes calldata extraOptions, bool payInLzToken)
        external
        view
        returns (MessagingFee memory fee)
    {
        bytes memory message = BridgeMsgCodec.encodeMarkQualified(seriesId);
        bytes memory options = combineOptions(BNB_EID, BridgeMsgCodec.MSG_MARK_QUALIFIED, extraOptions);
        return _quote(BNB_EID, message, options, payInLzToken);
    }

    // --- Send Functions ---
    /// @inheritdoc IOriginMessenger
    function sendAuctionStageStart(
        AuctionStageStartParams calldata params,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable onlyRole(DESIS_ROLE) returns (MessagingReceipt memory receipt) {
        bytes memory message = _encodeAuctionStageStart(params);
        bytes memory options = combineOptions(BNB_EID, BridgeMsgCodec.MSG_AUCTION_STAGE_START, extraOptions);

        receipt = _lzSend(BNB_EID, message, options, fee, refundAddress);
        emit AuctionStageSent(receipt.guid, params.seriesId, BridgeMsgCodec.MSG_AUCTION_STAGE_START);
    }

    /// @inheritdoc IOriginMessenger
    function sendAuctionStageReveal(
        uint32 seriesId,
        bool isGreenDay,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable onlyRole(DESIS_ROLE) returns (MessagingReceipt memory receipt) {
        bytes memory message = BridgeMsgCodec.encodeAuctionStageReveal(seriesId, isGreenDay);
        bytes memory options = combineOptions(BNB_EID, BridgeMsgCodec.MSG_AUCTION_STAGE_REVEAL, extraOptions);

        receipt = _lzSend(BNB_EID, message, options, fee, refundAddress);
        emit AuctionStageSent(receipt.guid, seriesId, BridgeMsgCodec.MSG_AUCTION_STAGE_REVEAL);
    }

    /// @inheritdoc IOriginMessenger
    function sendAuctionStageClearing(
        uint32 seriesId,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable onlyRole(DESIS_ROLE) returns (MessagingReceipt memory receipt) {
        bytes memory message = BridgeMsgCodec.encodeAuctionStageClearing(seriesId);
        bytes memory options = combineOptions(BNB_EID, BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING, extraOptions);

        receipt = _lzSend(BNB_EID, message, options, fee, refundAddress);
        emit AuctionStageSent(receipt.guid, seriesId, BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING);
    }

    /// @inheritdoc IOriginMessenger
    function sendAuctionResult(
        uint32 seriesId,
        uint32 issuedIntexCount,
        uint64 auctionClearingRate,
        uint32 wonBidsCount,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable onlyRole(DESIS_ROLE) returns (MessagingReceipt memory receipt) {
        bytes memory message = BridgeMsgCodec.encodeAuctionResult(
                seriesId, issuedIntexCount, auctionClearingRate, wonBidsCount
            );
        bytes memory options = combineOptions(BNB_EID, BridgeMsgCodec.MSG_AUCTION_RESULT, extraOptions);

        receipt = _lzSend(BNB_EID, message, options, fee, refundAddress);
        emit AuctionResultSent(receipt.guid, seriesId, issuedIntexCount, auctionClearingRate);
    }

    /// @inheritdoc IOriginMessenger
    function sendIssuanceInstructions(
        IssuanceInstructionsParams calldata params,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable onlyRole(INTEX_FACTORY_ROLE) returns (MessagingReceipt memory receipt) {
        uint256 len = params.recipients.length;
        if (len == 0) revert EmptyArray();
        if (len != params.quantities.length) revert ArrayLengthMismatch();

        receipt = _lzSend(
            BNB_EID,
            BridgeMsgCodec.encodeIssuanceInstructions(_toCodecPayload(params)),
            _issuanceReceiveOption(len),
            fee,
            refundAddress
        );
        emit IssuanceInstructionsSent(receipt.guid, params.seriesId, len);
    }

    /// @inheritdoc IOriginMessenger
    function sendRefundInstructions(
        uint32 seriesId,
        address[] calldata bidders,
        uint128[] calldata refundedAmounts,
        uint128[] calldata paidAmounts,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable onlyRole(DESIS_ROLE) returns (MessagingReceipt memory receipt) {
        uint256 len = bidders.length;
        if (len == 0) revert EmptyArray();
        if (len != refundedAmounts.length || len != paidAmounts.length) revert ArrayLengthMismatch();

        bytes memory message = BridgeMsgCodec.encodeRefundInstructions(seriesId, bidders, refundedAmounts, paidAmounts);
        bytes memory options = _refundReceiveOption(len);

        receipt = _lzSend(BNB_EID, message, options, fee, refundAddress);
        emit RefundInstructionsSent(receipt.guid, seriesId, len);
    }

    /// @inheritdoc IOriginMessenger
    function sendMarkCalled(
        uint32 seriesId,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable onlyRole(INTEX_FACTORY_ROLE) returns (MessagingReceipt memory receipt) {
        bytes memory message = BridgeMsgCodec.encodeMarkCalled(seriesId);
        bytes memory options = combineOptions(BNB_EID, BridgeMsgCodec.MSG_MARK_CALLED, extraOptions);

        receipt = _lzSend(BNB_EID, message, options, fee, refundAddress);
        emit MarkCalledSent(receipt.guid, seriesId);
    }

    /// @inheritdoc IOriginMessenger
    function sendMarkQualified(
        uint32 seriesId,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable onlyRole(INTEX_FACTORY_ROLE) returns (MessagingReceipt memory receipt) {
        bytes memory message = BridgeMsgCodec.encodeMarkQualified(seriesId);
        bytes memory options = combineOptions(BNB_EID, BridgeMsgCodec.MSG_MARK_QUALIFIED, extraOptions);

        receipt = _lzSend(BNB_EID, message, options, fee, refundAddress);
        emit MarkQualifiedSent(receipt.guid, seriesId);
    }

    // --- Receive ---
    /// @notice LayerZero endpoint entry point: records the ORDERED nonce and dispatches the inbound
    ///         message from BNB to its handler by msgType.
    /// @dev Validation order: bump per-channel nonce → self-call `dispatchInbound` (header length →
    ///      per-type minimum length → dispatch). `nonReentrant` protects against re-entry through the
    ///      Desis recipient (e.g. a hostile downstream callback into another `_lzReceive` entry point).
    ///      The trailing executor and extraData arguments are unused by this adapter.
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
        // Bump the per-channel nonce so `nextNonce` advances by exactly one. Endpoint already
        // verified `_origin.nonce == inboundNonce + 1` before calling us; recording here keeps the
        // invariant for the next delivery on this `(srcEid, sender)` channel.
        _s().inboundNonce[_origin.srcEid][_origin.sender] = _origin.nonce;

        // Drop-don't-block: the nonce is already advanced, so a deterministic revert in decode or the
        // downstream Desis call must not escape `_lzReceive` and wedge the ORDERED lane.
        try this.dispatchInbound(_guid, _origin.srcEid, _message) {}
        catch (bytes memory reason) {
            emit InboundMessageDropped(_guid, _origin.srcEid, reason);
        }
    }

    /// @notice Self-call shim that decodes and dispatches an inbound message by msgType.
    /// @dev Restricted to self (`msg.sender == address(this)`) so it can be invoked via `this.` from
    ///      `_lzReceive`, isolating the dispatch in its own call frame. `_lzReceive` wraps this call in
    ///      a try/catch and emits `InboundMessageDropped` on revert, so a malformed message cannot stall
    ///      the ORDERED channel (the nonce is already advanced).
    /// @param _guid Unique LayerZero message identifier.
    /// @param _srcEid LayerZero source endpoint id from `_origin`.
    /// @param _message Encoded bridge payload (header + body).
    function dispatchInbound(bytes32 _guid, uint32 _srcEid, bytes calldata _message) external {
        if (msg.sender != address(this)) revert NotSelf();

        uint8 msgType = BridgeMsgCodec.readHeader(_message);
        BridgeMsgCodec.assertMinLength(_message, msgType);

        if (msgType == BridgeMsgCodec.MSG_BIDS_BATCH) {
            _handleBidsBatch(_guid, _srcEid, _message);
        } else {
            revert BridgeMsgCodec.UnknownMsgType(msgType);
        }
    }

    /// @notice Decode a BIDS_BATCH payload, forward it to the Desis recipient, and auto-fire clearing
    ///         when the series is ready.
    /// @dev The body carries its own `srcEid` field; it is cross-checked against the transport-layer
    ///      `_origin.srcEid` so a peer registered for chain X cannot claim a payload originated
    ///      on chain Y (`SrcEidBodyMismatch` reverts the inbound packet). After `processBidsBatch`,
    ///      if the series stage is `BidsReceived` the auction is cleared and `ClearingAutoDispatched`
    ///      is emitted; a clearing-side revert is caught locally and surfaced as
    ///      `ClearingAutoDispatchFailed` without rolling back the bid intake.
    /// @param _guid Unique message identifier
    /// @param _originSrcEid LayerZero source endpoint id from `_origin`
    /// @param _message Encoded bids batch payload
    function _handleBidsBatch(bytes32 _guid, uint32 _originSrcEid, bytes calldata _message) internal {
        (
            uint32 seriesId,
            uint32 bodySrcEid,
            bool isLast,
            uint32 relayGeneration,
            address[] memory bidderAddresses,
            uint16[] memory intexQuantities,
            uint32[] memory intexBidRates,
            uint32[] memory timestamps
        ) = BridgeMsgCodec.decodeBidsBatch(_message);

        if (bodySrcEid != _originSrcEid) revert SrcEidBodyMismatch(_originSrcEid, bodySrcEid);

        address desisRecipient = _s().desis;
        IDesis(desisRecipient)
            .processBidsBatch(
                seriesId,
                _originSrcEid,
                isLast,
                relayGeneration,
                bidderAddresses,
                intexQuantities,
                intexBidRates,
                timestamps
            );

        emit BidsBatchReceived(_guid, _originSrcEid, seriesId, bidderAddresses.length);

        // Auto-fire clearing once bids are committed. Local try/catch so a clearing-side revert
        // does not roll back the bid intake — operators can retry clearAuction manually if needed.
        if (IDesis(desisRecipient).getAuctionStage(seriesId) == IDesis.AuctionStage.BidsReceived) {
            try IDesis(desisRecipient).clearAuction(seriesId) {
                emit ClearingAutoDispatched(seriesId);
            } catch (bytes memory reason) {
                emit ClearingAutoDispatchFailed(seriesId, reason);
            }
        }
    }

    // --- Internal helpers ---
    /// @notice Next expected inbound nonce for ORDERED LayerZero delivery on a `(srcEid, sender)` channel.
    /// @dev Override returns `inboundNonce + 1`. The endpoint refuses to route any packet whose
    ///      `_origin.nonce` does not equal this value, so duplicates and out-of-order deliveries are
    ///      rejected at the transport layer before `_lzReceive` runs.
    /// @param _srcEid LayerZero source endpoint id of the channel.
    /// @param _sender Bytes32-encoded peer address on the source chain.
    /// @return The next inbound nonce the endpoint must deliver on this channel.
    function nextNonce(uint32 _srcEid, bytes32 _sender) public view override returns (uint64) {
        return _s().inboundNonce[_srcEid][_sender] + 1;
    }

    /// @notice ERC-165 support check, resolving the AccessControl interface ids.
    /// @param interfaceId Interface id to check.
    /// @return True if the interface is supported.
    function supportsInterface(bytes4 interfaceId) public view override(AccessControlUpgradeable) returns (bool) {
        return super.supportsInterface(interfaceId);
    }

    /// @notice Pay the LayerZero native fee from either the relay float or caller-supplied value.
    /// @dev Distinguishes two funding modes (mirrors `TargetMessenger._payNative`):
    ///        * relay-funded (`msg.value == 0`): the send originates from a chain-native module that
    ///          cannot attach value. Pay from the contract's pre-funded native float.
    ///        * entry-funded (`msg.value > 0`): an operator supplied value against a quoted fee.
    ///          Require `msg.value >= fee` and refund any excess to the caller, so an entry caller's
    ///          buffer never silently seeds (or, if short, drains) the relay float.
    ///      Dormant until the float is funded — with no float and no value the send still reverts,
    ///      so the entry-funded EOA flow is unchanged.
    /// @param _nativeFee Required native fee amount
    /// @return nativeFee Actual fee paid
    function _payNative(uint256 _nativeFee) internal override returns (uint256 nativeFee) {
        if (msg.value == 0) {
            if (address(this).balance < _nativeFee) revert NotEnoughNative(address(this).balance);
            return _nativeFee;
        }

        if (msg.value < _nativeFee) revert MsgValueBelowFee(msg.value, _nativeFee);

        uint256 refund = msg.value - _nativeFee;
        if (refund > 0) {
            // Refund excess back to the entry caller so it does not silently seed the relay float.
            // slither-disable-next-line arbitrary-send-eth
            (bool ok,) = msg.sender.call{value: refund}("");
            if (!ok) revert RefundFailed();
        }
        return _nativeFee;
    }

    /// @notice Accept native to pre-fund the relay float (and receive LayerZero fee refunds).
    receive() external payable {}

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
