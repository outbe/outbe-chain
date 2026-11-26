// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {IERC7786Recipient} from "@openzeppelin/contracts/interfaces/draft-IERC7786.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {SafeCast} from "@openzeppelin/contracts/utils/math/SafeCast.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";

import {IERC1155Bridgeable} from "./interfaces/IERC1155Bridgeable.sol";
import {
    IIntexNFT1155Router,
    SendParam,
    BatchSendParam,
    MultiRecipientSendParam
} from "./interfaces/IIntexNFT1155Router.sol";
import {IntexNFT1155RouterCodec} from "./libs/IntexNFT1155RouterCodec.sol";
import {ERC7786Sender} from "./ERC7786Sender.sol";
import {IntexGas} from "./libs/IntexGas.sol";

/// @title IntexNFT1155Router
/// @author Outbe
/// @notice Batch cross-chain ERC-1155 adapter over the protocol-agnostic ERC-7786 bridge. It burns
///         on the source and mints on the paired adapter registered as the remote router for a
///         chainId.
/// @dev UUPS upgradeable. The bridge and bridged token are implementation immutables. Modes:
///      single-recipient batch and multi-recipient.
contract IntexNFT1155Router is
    IIntexNFT1155Router,
    ERC7786Sender,
    IERC7786Recipient,
    AccessControlUpgradeable,
    ReentrancyGuardTransient,
    UUPSUpgradeable
{
    using InteroperableAddress for bytes;

    uint16 public constant SEND_ONE = IntexNFT1155RouterCodec.SEND_ONE;
    uint16 public constant SEND_BATCH = IntexNFT1155RouterCodec.SEND_BATCH;
    uint16 public constant SEND_MULTI = IntexNFT1155RouterCodec.SEND_MULTI;
    uint8 public constant BODY_VERSION_V1 = IntexNFT1155RouterCodec.BODY_VERSION_V1;
    uint256 public constant MAX_BATCH_SIZE = IntexNFT1155RouterCodec.MAX_BATCH_SIZE;

    /// @notice The bridgeable ERC-1155 this adapter burns on send and mints on receive.
    IERC1155Bridgeable public immutable token;

    /// @notice Snapshot of one batch item whose `token.crosschainMint` reverted. `exists`
    ///         distinguishes never-failed from failed-and-retried. The struct does not keep the
    ///         revert data: neither `retryIntexCrosschainReceive` nor `sendBackToSource` reads it.
    ///         `IntexCrosschainReceiveFailed` carries it for whoever diagnoses the failure. The field order
    ///         lets the address, chain and flag share a slot.
    struct FailedCrosschainReceive {
        address to;
        uint32 srcChainId;
        bool exists;
        uint256 intexId;
        uint256 units;
    }

    /// @custom:storage-location erc7201:outbe.intex.IntexNFT1155Router
    struct IntexNFT1155RouterStorage {
        /// @dev Per-message map of items whose `token.crosschainMint` reverted.
        mapping(bytes32 receiveId => mapping(uint256 idx => FailedCrosschainReceive)) failedCrosschainReceives;
        /// @dev ERC-7930 interoperable address of the matching adapter on a given chainId.
        mapping(uint32 chainId => bytes interop) remoteRouter;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.IntexNFT1155Router")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0x2cbe3762cb0a38b88ca9f61f43618d2924a64d49c5fd918e01bf25761a81cd00;

    function _bs() private pure returns (IntexNFT1155RouterStorage storage $) {
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address _token, address bridge_) ERC7786Sender(bridge_) {
        if (_token == address(0)) revert ZeroAddress("token");
        token = IERC1155Bridgeable(_token);
        _disableInitializers();
    }

    /// @notice Initializes the proxy admin.
    function initialize(address _delegate) external initializer {
        if (_delegate == address(0)) revert ZeroAddress("delegate");
        __AccessControl_init();
        _grantRole(DEFAULT_ADMIN_ROLE, _delegate);
    }

    function _authorizeUpgrade(address newImplementation) internal override onlyRole(DEFAULT_ADMIN_ROLE) {}

    /// @notice Failed crosschainMint snapshot for item `idx` of the message `receiveId`.
    function getFailedCrosschainReceive(bytes32 receiveId, uint256 idx)
        external
        view
        returns (address to, uint256 intexId, uint256 units, bool exists)
    {
        FailedCrosschainReceive storage f = _bs().failedCrosschainReceives[receiveId][idx];
        return (f.to, f.intexId, f.units, f.exists);
    }

    function supportsInterface(bytes4 interfaceId) public view override(AccessControlUpgradeable) returns (bool) {
        return interfaceId == type(IIntexNFT1155Router).interfaceId || super.supportsInterface(interfaceId);
    }

    /// @inheritdoc IIntexNFT1155Router
    function setRemoteRouter(uint32 chainId, bytes calldata interop) external onlyRole(DEFAULT_ADMIN_ROLE) {
        // Inbound auth derives the key from the sender's own bytes, so the interop must embed the
        // same chainId as the key: a mismatch would silently drop every message from that chain.
        if (interop.length != 0) {
            (uint256 embedded,) = interop.parseEvmV1Calldata();
            require(embedded == chainId, RemoteRouterChainMismatch(chainId, embedded));
        }
        _bs().remoteRouter[chainId] = interop;
        emit RemoteRouterSet(chainId, interop);
    }

    /// @notice ERC-7930 interoperable address of the matching adapter on `chainId` (empty if unregistered).
    function remoteRouter(uint32 chainId) external view returns (bytes memory) {
        return _bs().remoteRouter[chainId];
    }

    function _remoteRouter(uint32 chainId) private view returns (bytes memory interop) {
        interop = _bs().remoteRouter[chainId];
        require(interop.length != 0, RemoteRouterNotSet(chainId));
    }

    /// @dev The caller always pays the fee; the native float is never used.
    function _sendPaidByCaller(uint32 dstChainId, bytes memory payload, uint256 gasLimit) private returns (bytes32) {
        return _sendToPaidByCaller(_remoteRouter(dstChainId), payload, gasLimit);
    }

    function _quoteFee(uint32 dstChainId, bytes memory payload, uint256 gasLimit) private view returns (uint256) {
        return _quoteTo(_remoteRouter(dstChainId), payload, gasLimit);
    }

    // --- Single transfer ---
    /// @inheritdoc IIntexNFT1155Router
    function quoteSend(SendParam calldata _sendParam) external view returns (uint256) {
        return _quoteFee(_sendParam.dstChainId, _buildSingleMsg(_sendParam), IntexGas.nftMint(1));
    }

    /// @inheritdoc IIntexNFT1155Router
    function send(SendParam calldata _sendParam) external payable nonReentrant returns (bytes32 sendId) {
        bytes memory message = _buildSingleMsg(_sendParam);
        token.crosschainBurn(msg.sender, _toAddress(_sendParam.to), _sendParam.intexId, _sendParam.units);
        sendId = _sendPaidByCaller(_sendParam.dstChainId, message, IntexGas.nftMint(1));
        emit IntexCrosschainSent(
            sendId, _sendParam.dstChainId, msg.sender, _sendParam.to, _sendParam.intexId, _units32(_sendParam.units)
        );
    }

    /// @dev `assertAddress` has already rejected anything not address-shaped by the time this narrows one.
    function _toAddress(bytes32 recipient) internal pure returns (address) {
        return address(uint160(uint256(recipient)));
    }

    function _buildSingleMsg(SendParam calldata _sendParam) internal pure returns (bytes memory) {
        // reject a malformed recipient before burning. The receive path rejects it too.
        IntexNFT1155RouterCodec.assertAddress(_sendParam.to);
        if (_sendParam.to == bytes32(0)) revert InvalidReceiver();
        return IntexNFT1155RouterCodec.encodeSingle(
            IntexNFT1155RouterCodec.SinglePayload({
                to: _sendParam.to, intexId: _sendParam.intexId, units: _sendParam.units
            })
        );
    }

    // --- Single-recipient batch ---
    /// @inheritdoc IIntexNFT1155Router
    function quoteBatchSend(BatchSendParam calldata _sendParam) external view returns (uint256) {
        return
            _quoteFee(_sendParam.dstChainId, _buildBatchMsg(_sendParam), IntexGas.nftMint(_sendParam.intexIds.length));
    }

    /// @inheritdoc IIntexNFT1155Router
    function batchSend(BatchSendParam calldata _sendParam) external payable nonReentrant returns (bytes32 sendId) {
        if (_sendParam.intexIds.length == 0) revert EmptyBatch();
        if (_sendParam.intexIds.length != _sendParam.units.length) revert ArrayLengthMismatch();

        // Build first: the zero-`to` and `MAX_BATCH_SIZE` guards fail fast before any burn.
        bytes memory message = _buildBatchMsg(_sendParam);
        for (uint256 i = 0; i < _sendParam.intexIds.length; i++) {
            token.crosschainBurn(msg.sender, _toAddress(_sendParam.to), _sendParam.intexIds[i], _sendParam.units[i]);
        }

        sendId = _sendPaidByCaller(_sendParam.dstChainId, message, IntexGas.nftMint(_sendParam.intexIds.length));
        emit IntexCrosschainBatchSent(
            sendId, _sendParam.dstChainId, msg.sender, _sendParam.to, _sendParam.intexIds, _units32(_sendParam.units)
        );
    }

    function _buildBatchMsg(BatchSendParam calldata _sendParam) internal pure returns (bytes memory) {
        IntexNFT1155RouterCodec.assertAddress(_sendParam.to);
        if (_sendParam.to == bytes32(0)) revert InvalidReceiver();
        if (_sendParam.intexIds.length > MAX_BATCH_SIZE) {
            revert IntexNFT1155RouterCodec.BatchTooLarge(_sendParam.intexIds.length, MAX_BATCH_SIZE);
        }
        return IntexNFT1155RouterCodec.encodeBatch(
            IntexNFT1155RouterCodec.BatchPayload({
                to: _sendParam.to, intexIds: _sendParam.intexIds, units: _sendParam.units
            })
        );
    }

    // --- Multi-recipient batch ---
    /// @inheritdoc IIntexNFT1155Router
    function quoteMultiSend(MultiRecipientSendParam calldata _sendParam) external view returns (uint256) {
        return
            _quoteFee(_sendParam.dstChainId, _buildMultiMsg(_sendParam), IntexGas.nftMint(_sendParam.recipients.length));
    }

    /// @inheritdoc IIntexNFT1155Router
    function multiSend(MultiRecipientSendParam calldata _sendParam)
        external
        payable
        nonReentrant
        returns (bytes32 sendId)
    {
        uint256 len = _sendParam.recipients.length;
        if (len == 0) revert EmptyBatch();
        if (len != _sendParam.intexIds.length || len != _sendParam.units.length) revert ArrayLengthMismatch();

        bytes memory message = _buildMultiMsg(_sendParam);
        for (uint256 i = 0; i < len; i++) {
            IntexNFT1155RouterCodec.assertAddress(_sendParam.recipients[i]);
            if (_sendParam.recipients[i] == bytes32(0)) revert InvalidReceiver();
            token.crosschainBurn(
                msg.sender, _toAddress(_sendParam.recipients[i]), _sendParam.intexIds[i], _sendParam.units[i]
            );
        }

        sendId = _sendPaidByCaller(_sendParam.dstChainId, message, IntexGas.nftMint(_sendParam.recipients.length));
        emit IntexCrosschainMultiSent(
            sendId,
            _sendParam.dstChainId,
            msg.sender,
            _sendParam.recipients,
            _sendParam.intexIds,
            _units32(_sendParam.units)
        );
    }

    function _buildMultiMsg(MultiRecipientSendParam calldata _sendParam) internal pure returns (bytes memory) {
        if (_sendParam.recipients.length > MAX_BATCH_SIZE) {
            revert IntexNFT1155RouterCodec.BatchTooLarge(_sendParam.recipients.length, MAX_BATCH_SIZE);
        }
        return IntexNFT1155RouterCodec.encodeMulti(
            IntexNFT1155RouterCodec.MultiPayload({
                recipients: _sendParam.recipients, intexIds: _sendParam.intexIds, units: _sendParam.units
            })
        );
    }

    // --- Receive ---
    /// @inheritdoc IERC7786Recipient
    /// @dev Only the bridge, with a message from the registered adapter of the source chain. The
    ///      bridge deduplicates and rolls back on revert, so a premature message is redelivered.
    ///      nonReentrant guards against crosschainMint-callback re-entry.
    function receiveMessage(bytes32 receiveId, bytes calldata sender, bytes calldata payload)
        external
        payable
        nonReentrant
        returns (bytes4)
    {
        require(msg.sender == address(BRIDGE), UnauthorizedBridge(msg.sender));
        (uint256 srcChainId,) = sender.parseEvmV1Calldata();
        uint32 chainId = SafeCast.toUint32(srcChainId);
        bytes memory expected = _bs().remoteRouter[chainId];
        require(
            expected.length != 0 && keccak256(sender) == keccak256(expected), UnauthorizedSourceRouter(chainId, sender)
        );
        _dispatch(chainId, receiveId, payload);
        return IERC7786Recipient.receiveMessage.selector;
    }

    function _dispatch(uint32 srcChainId, bytes32 receiveId, bytes calldata message) private {
        if (message.length < IntexNFT1155RouterCodec.HEADER_LEN) {
            revert IntexNFT1155RouterCodec.InvalidPayloadLength(message.length, IntexNFT1155RouterCodec.HEADER_LEN);
        }
        uint8 version = uint8(message[0]);
        if (version != BODY_VERSION_V1) revert IntexNFT1155RouterCodec.UnsupportedBodyVersion(version);
        uint8 msgType = uint8(message[1]);

        if (msgType == SEND_ONE) {
            _handleSingleReceive(srcChainId, receiveId, message);
        } else if (msgType == SEND_BATCH) {
            _handleBatchReceive(srcChainId, receiveId, message);
        } else if (msgType == SEND_MULTI) {
            _handleMultiReceive(srcChainId, receiveId, message);
        } else {
            revert UnknownMsgType(msgType);
        }
    }

    function _handleSingleReceive(uint32 srcChainId, bytes32 receiveId, bytes calldata message) internal {
        IntexNFT1155RouterCodec.SinglePayload memory p = IntexNFT1155RouterCodec.decodeSingle(message);

        IntexNFT1155RouterCodec.assertAddress(p.to);
        if (p.to == bytes32(0)) revert InvalidReceiver();
        address toAddress = address(uint160(uint256(p.to)));

        // A failed mint is recorded with IntexCrosschainReceiveFailed, and its retry emits the receive.
        if (_tryCrosschainReceiveOne(srcChainId, receiveId, 0, toAddress, p.intexId, p.units)) {
            emit IntexCrosschainReceived(receiveId, srcChainId, toAddress, p.intexId, _units32(p.units));
        }
    }

    function _handleBatchReceive(uint32 srcChainId, bytes32 receiveId, bytes calldata message) internal {
        IntexNFT1155RouterCodec.BatchPayload memory p = IntexNFT1155RouterCodec.decodeBatch(message);

        IntexNFT1155RouterCodec.assertAddress(p.to);
        if (p.to == bytes32(0)) revert InvalidReceiver();
        address toAddress = address(uint160(uint256(p.to)));

        for (uint256 i = 0; i < p.intexIds.length; i++) {
            _tryCrosschainReceiveOne(srcChainId, receiveId, i, toAddress, p.intexIds[i], p.units[i]);
        }

        emit IntexCrosschainBatchReceived(receiveId, srcChainId, toAddress, p.intexIds, _units32(p.units));
    }

    function _handleMultiReceive(uint32 srcChainId, bytes32 receiveId, bytes calldata message) internal {
        IntexNFT1155RouterCodec.MultiPayload memory p = IntexNFT1155RouterCodec.decodeMulti(message);

        address[] memory recipients = new address[](p.recipients.length);
        for (uint256 i = 0; i < p.recipients.length; i++) {
            IntexNFT1155RouterCodec.assertAddress(p.recipients[i]);
            if (p.recipients[i] == bytes32(0)) revert InvalidReceiver();
            recipients[i] = address(uint160(uint256(p.recipients[i])));
            _tryCrosschainReceiveOne(srcChainId, receiveId, i, recipients[i], p.intexIds[i], p.units[i]);
        }

        emit IntexCrosschainMultiReceived(receiveId, srcChainId, recipients, p.intexIds, _units32(p.units));
    }

    /// @dev Isolate a per-item `token.crosschainMint` revert. Park a snapshot for
    ///      `retryIntexCrosschainReceive` instead of failing the whole batch.
    function _tryCrosschainReceiveOne(
        uint32 srcChainId,
        bytes32 receiveId,
        uint256 idx,
        address to,
        uint256 intexId,
        uint256 units
    ) internal returns (bool minted) {
        try this.crosschainReceiveOne(to, intexId, units) {
            minted = true;
        } catch (bytes memory reason) {
            _bs().failedCrosschainReceives[receiveId][idx] =
                FailedCrosschainReceive({to: to, srcChainId: srcChainId, exists: true, intexId: intexId, units: units});
            emit IntexCrosschainReceiveFailed(receiveId, srcChainId, idx, to, intexId, _units32(units), reason);
        }
    }

    /// @notice Self-call shim so a per-item mint revert lands in `_tryCrosschainReceiveOne`'s catch. Self-only.
    function crosschainReceiveOne(address to, uint256 intexId, uint256 units) external {
        if (msg.sender != address(this)) revert NotSelf();
        token.crosschainMint(to, intexId, units);
    }

    /// @notice Permissionless retry of a previously-failed crosschainMint. A success clears the
    ///         entry.
    function retryIntexCrosschainReceive(bytes32 receiveId, uint256 idx) external nonReentrant {
        IntexNFT1155RouterStorage storage $ = _bs();
        FailedCrosschainReceive memory f = $.failedCrosschainReceives[receiveId][idx];
        if (!f.exists) revert NoSuchFailedCrosschainReceive(receiveId, idx);
        delete $.failedCrosschainReceives[receiveId][idx];
        token.crosschainMint(f.to, f.intexId, f.units);
        emit IntexCrosschainReceived(receiveId, f.srcChainId, f.to, f.intexId, _units32(f.units));
    }

    /// @notice Permissionless send-back of a batch item that the destination gate rejects
    ///         terminally. It re-mints the owner on the origin chain via a reverse one-item
    ///         transfer. This is the only exit that does not re-hit the destination lifecycle gate.
    ///         The caller pays the fee (see `quoteSendBackToSource`). It consumes the entry once (CEI delete first).
    /// @param receiveId Inbound message id where the item's crosschainMint is stranded.
    /// @param idx Position of the stranded item in that batch.
    function sendBackToSource(bytes32 receiveId, uint256 idx) external payable nonReentrant returns (bytes32 sendId) {
        IntexNFT1155RouterStorage storage $ = _bs();
        FailedCrosschainReceive memory f = $.failedCrosschainReceives[receiveId][idx];
        if (!f.exists) revert NoSuchFailedCrosschainReceive(receiveId, idx);
        delete $.failedCrosschainReceives[receiveId][idx];

        sendId = _sendPaidByCaller(f.srcChainId, _buildSendBackMsg(f), IntexGas.nftMint(1));
        emit IntexCrosschainSentBack(
            sendId, f.srcChainId, receiveId, idx, bytes32(uint256(uint160(f.to))), f.intexId, _units32(f.units)
        );
    }

    /// @dev Units of one series fit `uint32`: the token caps its supply there. A wider count, which
    ///      only a malformed message can carry, saturates, so a receive never reverts on its content.
    function _units32(uint256 units) private pure returns (uint32) {
        // forge-lint: disable-next-line(unsafe-typecast) -- at most type(uint32).max here
        return units > type(uint32).max ? type(uint32).max : uint32(units);
    }

    function _units32(uint256[] memory units) private pure returns (uint32[] memory narrow) {
        narrow = new uint32[](units.length);
        for (uint256 i = 0; i < units.length; i++) {
            narrow[i] = _units32(units[i]);
        }
    }

    /// @notice Native fee that `sendBackToSource(receiveId, idx)` takes.
    function quoteSendBackToSource(bytes32 receiveId, uint256 idx) external view returns (uint256) {
        FailedCrosschainReceive memory f = _bs().failedCrosschainReceives[receiveId][idx];
        if (!f.exists) revert NoSuchFailedCrosschainReceive(receiveId, idx);
        return _quoteFee(f.srcChainId, _buildSendBackMsg(f), IntexGas.nftMint(1));
    }

    /// @dev The reverse one-item transfer that returns a stranded item to its owner on the origin chain.
    function _buildSendBackMsg(FailedCrosschainReceive memory f) private pure returns (bytes memory) {
        bytes32[] memory recipients = new bytes32[](1);
        recipients[0] = bytes32(uint256(uint160(f.to)));
        uint256[] memory intexIds = new uint256[](1);
        intexIds[0] = f.intexId;
        uint256[] memory units = new uint256[](1);
        units[0] = f.units;
        return IntexNFT1155RouterCodec.encodeMulti(
            IntexNFT1155RouterCodec.MultiPayload({recipients: recipients, intexIds: intexIds, units: units})
        );
    }
}
