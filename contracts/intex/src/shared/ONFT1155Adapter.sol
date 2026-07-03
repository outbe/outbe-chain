// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";

import {IERC1155Bridgeable} from "./interfaces/IERC1155Bridgeable.sol";
import {IONFT1155Adapter, SendParam} from "./interfaces/IONFT1155Adapter.sol";
import {ONFT1155MsgCodec} from "./libs/ONFT1155MsgCodec.sol";
import {ERC7786MessengerBase} from "./ERC7786MessengerBase.sol";
import {IntexGas} from "./libs/IntexGas.sol";

/// @title ONFT1155Adapter
/// @author Outbe
/// @notice Single-token cross-chain ERC-1155 adapter over the protocol-agnostic ERC-7786 bridge: burns on the source
///         and mints on the paired adapter registered as the remote messenger for a chainId.
/// @dev UUPS upgradeable; the bridge and bridged token are implementation immutables.
contract ONFT1155Adapter is
    IONFT1155Adapter,
    ERC7786MessengerBase,
    AccessControlUpgradeable,
    ReentrancyGuardTransient,
    UUPSUpgradeable
{
    using ONFT1155MsgCodec for bytes;

    /// @notice The ERC-1155 token this adapter burns on send and mints on receive.
    IERC1155Bridgeable public immutable token;

    /// @notice Snapshot of an inbound transfer whose `token.crosschainMint` reverted; `exists` distinguishes
    ///         never-failed from failed-and-retried.
    struct FailedCrosschainMint {
        address to;
        uint256 tokenId;
        uint256 amount;
        uint32 srcChainId;
        bytes reason;
        bool exists;
    }

    /// @custom:storage-location erc7201:outbe.intex.ONFT1155Adapter
    struct ONFT1155AdapterStorage {
        /// @dev Inbound message ids already minted (defence-in-depth; the hub also dedups).
        mapping(bytes32 receiveId => bool) processed;
        /// @dev Inbound transfers whose `token.crosschainMint` reverted, keyed by the bridge message id.
        mapping(bytes32 receiveId => FailedCrosschainMint) failedCrosschainMints;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.ONFT1155Adapter")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0xf7ba3c8714f9cd40d66e510e06f778c613706e48a100857a0ad130fc96ece900;

    function _as() private pure returns (ONFT1155AdapterStorage storage $) {
        // solhint-disable-next-line no-inline-assembly
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @notice Inbound message with this `receiveId` has already been minted.
    error AlreadyProcessed(bytes32 receiveId);
    /// @notice `crosschainMintOne` was invoked by an external caller; only `address(this)` is allowed.
    error NotSelf();
    /// @notice No failed-crosschainMint entry exists for `receiveId`.
    error NoSuchFailedCrosschainMint(bytes32 receiveId);
    /// @notice Parked entry carries no origin chainId (pre-upgrade entry); reclaim cannot route back.
    error NoReclaimSource(bytes32 receiveId);

    /// @notice Emitted when an inbound transfer's `token.crosschainMint` reverts and is parked for retry.
    event CrosschainMintFailed(
        uint32 indexed srcChainId,
        bytes32 indexed receiveId,
        address indexed to,
        uint256 tokenId,
        uint256 amount,
        bytes reason
    );

    /// @notice Emitted when `retryCrosschainMint` successfully mints a previously failed transfer.
    event CrosschainMintRetried(bytes32 indexed receiveId);

    /// @notice Emitted when a terminally-failed crosschainMint is reclaimed to its origin chain for re-mint.
    event CrosschainMintReclaimed(
        bytes32 indexed receiveId, uint32 indexed srcChainId, address indexed to, uint256 tokenId, uint256 amount
    );

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address _token, address bridge_) ERC7786MessengerBase(bridge_) {
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

    // solhint-disable-next-line no-empty-blocks
    function _authorizeUpgrade(address newImplementation) internal override onlyRole(DEFAULT_ADMIN_ROLE) {}

    /// @notice Failed crosschainMint snapshot for the message `receiveId`.
    function failedCrosschainMints(bytes32 receiveId)
        external
        view
        returns (address to, uint256 tokenId, uint256 amount, bytes memory reason, bool exists)
    {
        FailedCrosschainMint storage f = _as().failedCrosschainMints[receiveId];
        return (f.to, f.tokenId, f.amount, f.reason, f.exists);
    }

    function supportsInterface(bytes4 interfaceId) public view override(AccessControlUpgradeable) returns (bool) {
        return interfaceId == type(IONFT1155Adapter).interfaceId || super.supportsInterface(interfaceId);
    }

    /// @inheritdoc IONFT1155Adapter
    function setRemoteMessenger(uint32 chainId, bytes calldata interop) external onlyRole(DEFAULT_ADMIN_ROLE) {
        _setRemoteMessenger(chainId, interop);
    }

    // --- Send ---
    /// @inheritdoc IONFT1155Adapter
    function quoteSend(SendParam calldata _sendParam) external view returns (uint256) {
        return _quoteFee(_sendParam.dstChainId, _buildMsg(_sendParam), IntexGas.onftMint(1));
    }

    /// @inheritdoc IONFT1155Adapter
    function send(SendParam calldata _sendParam) external payable nonReentrant returns (bytes32 sendId) {
        // Build first: the zero-`to` guard fails fast before the burn.
        bytes memory message = _buildMsg(_sendParam);
        token.crosschainBurn(msg.sender, _sendParam.tokenId, _sendParam.amount);

        sendId = _send(_sendParam.dstChainId, message, IntexGas.onftMint(1));
        emit ONFTSent(sendId, _sendParam.dstChainId, msg.sender, _sendParam.tokenId, _sendParam.amount);
    }

    function _buildMsg(SendParam calldata _sendParam) internal view returns (bytes memory message) {
        if (_sendParam.to == bytes32(0)) revert InvalidReceiver();
        // Compose is dropped; the empty tail keeps `encode` on its plain-transfer path.
        (message,) = ONFT1155MsgCodec.encode(_sendParam.to, _sendParam.tokenId, _sendParam.amount, "");
    }

    // --- Receive ---
    /// @inheritdoc ERC7786MessengerBase
    /// @dev nonReentrant guards against crosschainMint-callback re-entry.
    function receiveMessage(bytes32 receiveId, bytes calldata sender, bytes calldata payload)
        public
        payable
        override
        nonReentrant
        returns (bytes4)
    {
        return super.receiveMessage(receiveId, sender, payload);
    }

    function _dispatch(uint32 srcChainId, bytes32 receiveId, bytes calldata message) internal override {
        ONFT1155AdapterStorage storage $ = _as();
        if ($.processed[receiveId]) revert AlreadyProcessed(receiveId);
        $.processed[receiveId] = true;

        // A valid transfer is exactly MIN_LEN_TRANSFER bytes; reject any other length.
        if (message.length != ONFT1155MsgCodec.MIN_LEN_TRANSFER) {
            revert ONFT1155MsgCodec.InvalidPayloadLength(message.length, ONFT1155MsgCodec.MIN_LEN_TRANSFER);
        }
        bytes32 sendToRaw = message.sendTo();
        ONFT1155MsgCodec.assertAddress(sendToRaw);
        address toAddress = ONFT1155MsgCodec.bytes32ToAddress(sendToRaw);
        uint256 tokenId_ = message.tokenId();
        uint256 amount_ = message.amount();

        // Isolate the mint: a revert (e.g. a series past its settlement deadline) parks the transfer for retry
        // instead of unwinding the packet and stranding burned tokens.
        try this.crosschainMintOne(toAddress, tokenId_, amount_) {
            emit ONFTReceived(receiveId, srcChainId, toAddress, tokenId_, amount_);
        } catch (bytes memory reason) {
            $.failedCrosschainMints[receiveId] = FailedCrosschainMint({
                to: toAddress, tokenId: tokenId_, amount: amount_, srcChainId: srcChainId, reason: reason, exists: true
            });
            emit CrosschainMintFailed(srcChainId, receiveId, toAddress, tokenId_, amount_, reason);
        }
    }

    /// @notice Self-call shim so a mint revert lands in `_dispatch`'s catch. Self-only.
    function crosschainMintOne(address to, uint256 tokenId, uint256 amount) external {
        if (msg.sender != address(this)) revert NotSelf();
        token.crosschainMint(to, tokenId, amount);
    }

    /// @notice Permissionless retry of a transfer whose inbound crosschainMint failed; the entry is cleared on success.
    function retryCrosschainMint(bytes32 receiveId) external nonReentrant {
        ONFT1155AdapterStorage storage $ = _as();
        FailedCrosschainMint memory f = $.failedCrosschainMints[receiveId];
        if (!f.exists) revert NoSuchFailedCrosschainMint(receiveId);
        delete $.failedCrosschainMints[receiveId];
        token.crosschainMint(f.to, f.tokenId, f.amount);
        emit CrosschainMintRetried(receiveId);
    }

    /// @notice Permissionless reclaim of a crosschainMint the destination gate rejects terminally: re-mints the
    ///         holder on the origin chain via a reverse transfer, the only exit that does not re-hit the
    ///         destination lifecycle gate. Caller-funded; consumes the entry once (CEI delete first).
    /// @param receiveId Inbound message id whose crosschainMint is stranded.
    function reclaimToSource(bytes32 receiveId) external payable nonReentrant returns (bytes32 sendId) {
        ONFT1155AdapterStorage storage $ = _as();
        FailedCrosschainMint memory f = $.failedCrosschainMints[receiveId];
        if (!f.exists) revert NoSuchFailedCrosschainMint(receiveId);
        if (f.srcChainId == 0) revert NoReclaimSource(receiveId);
        delete $.failedCrosschainMints[receiveId];

        (bytes memory message,) =
            ONFT1155MsgCodec.encode(ONFT1155MsgCodec.addressToBytes32(f.to), f.tokenId, f.amount, "");
        sendId = _send(f.srcChainId, message, IntexGas.onftMint(1));
        emit CrosschainMintReclaimed(receiveId, f.srcChainId, f.to, f.tokenId, f.amount);
    }

    /// @inheritdoc IONFT1155Adapter
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
