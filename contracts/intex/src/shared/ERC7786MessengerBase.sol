// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IERC7786Recipient} from "@openzeppelin/contracts/interfaces/draft-IERC7786.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {SafeCast} from "@openzeppelin/contracts/utils/math/SafeCast.sol";
import {ERC7786Sender} from "./ERC7786Sender.sol";

/// @title ERC7786MessengerBase
/// @author Outbe
/// @notice Shared base for intex clients that both send and receive through the ERC-7786 bridge
///         (the `crosschain` hub's `ERC7786Bridge`), with a matching messenger on each chain.
/// @dev Provides, on top of {ERC7786Sender}:
///      - an ERC-7930 remote-messenger registry with chainId as key.
///      - {_send} and {_quoteFee} to the registered messenger of a chain.
///      - an authenticated {receiveMessage} that dispatches to the abstract {_dispatch}.
///
///      Upgrade-safe: the registry lives in erc7201 namespaced storage.
abstract contract ERC7786MessengerBase is ERC7786Sender, IERC7786Recipient {
    using InteroperableAddress for bytes;

    /// @custom:storage-location erc7201:outbe.intex.ERC7786MessengerBase
    struct MessengerStorage {
        /// @dev ERC-7930 interoperable address of the matching messenger on a given chainId.
        mapping(uint32 chainId => bytes interop) remoteMessenger;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.ERC7786MessengerBase")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _MESSENGER_STORAGE_SLOT =
        0x6702aa1076aac9174f7ad82f658a24b43715e5376566adbcf76ac99f64da8e00;

    event RemoteMessengerRegistered(uint32 indexed chainId, bytes interop);

    error UnauthorizedBridge(address caller);
    error UnauthorizedSourceMessenger(uint32 chainId, bytes sender);
    error RemoteMessengerNotSet(uint32 chainId);
    error RemoteMessengerChainMismatch(uint32 chainId, uint256 embeddedChainId);

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address bridge_) ERC7786Sender(bridge_) {}

    function _s() private pure returns (MessengerStorage storage $) {
        assembly ("memory-safe") {
            $.slot := _MESSENGER_STORAGE_SLOT
        }
    }

    // --- Remote messenger registry ---

    /// @notice ERC-7930 interoperable address of the matching messenger on `chainId` (empty if unregistered).
    /// @param chainId Destination/source EVM chainId (the bridge maps chainId to its transport id).
    /// @return The registered interoperable address, or empty bytes.
    function remoteMessenger(uint32 chainId) external view returns (bytes memory) {
        return _s().remoteMessenger[chainId];
    }

    /// @dev Registers (or clears, with empty bytes) the matching messenger on `chainId`. The
    ///      concrete contract is responsible for auth (AccessControl lives there).
    function _registerRemoteMessenger(uint32 chainId, bytes calldata interop) internal {
        // Inbound auth derives the key from the sender's own bytes, so the interop must embed the
        // same chainId as the key. A mismatch would silently blackhole every message from that
        // chain.
        if (interop.length != 0) {
            (uint256 embedded,) = interop.parseEvmV1Calldata();
            require(embedded == chainId, RemoteMessengerChainMismatch(chainId, embedded));
        }
        _s().remoteMessenger[chainId] = interop;
        emit RemoteMessengerRegistered(chainId, interop);
    }

    /// @dev Registered interoperable address of the matching messenger on `chainId`. Reverts if
    ///      never set.
    function _remoteMessenger(uint32 chainId) internal view returns (bytes memory interop) {
        interop = _s().remoteMessenger[chainId];
        require(interop.length != 0, RemoteMessengerNotSet(chainId));
    }

    // --- Outbound ---

    /// @dev Sends `payload` to the matching messenger on `dstChainId`, as {ERC7786Sender-_sendTo}.
    function _send(uint32 dstChainId, bytes memory payload, uint256 gasLimit) internal returns (bytes32) {
        return _sendTo(_remoteMessenger(dstChainId), payload, gasLimit);
    }

    /// @dev `_send` for a user entry point: the caller always pays the fee, and the native float is never used.
    function _sendPaidByCaller(uint32 dstChainId, bytes memory payload, uint256 gasLimit) internal returns (bytes32) {
        return _sendToPaidByCaller(_remoteMessenger(dstChainId), payload, gasLimit);
    }

    /// @dev Native fee to deliver `payload` to the matching messenger on `dstChainId` with `gasLimit` destination gas.
    function _quoteFee(uint32 dstChainId, bytes memory payload, uint256 gasLimit) internal view returns (uint256) {
        return _quoteTo(_remoteMessenger(dstChainId), payload, gasLimit);
    }

    // --- Inbound ---

    /// @inheritdoc IERC7786Recipient
    /// @dev The {BRIDGE} calls this with a message from the matching messenger on the source chain.
    ///      Authenticates the caller (the bridge) and the inner `sender` (the registered peer for
    ///      its chainId) before it dispatches. The bridge already deduplicates and rolls back on
    ///      revert. Thus a premature message simply reverts here, and the transport redelivers it
    ///      after its prerequisite lands.
    function receiveMessage(bytes32 receiveId, bytes calldata sender, bytes calldata payload)
        public
        payable
        virtual
        returns (bytes4)
    {
        require(msg.sender == address(BRIDGE), UnauthorizedBridge(msg.sender));

        (uint256 srcChainId,) = sender.parseEvmV1Calldata();
        // Only uint32 chainIds are supported (the registry key). The contract rejects an
        // out-of-range source outright.
        uint32 chainId = SafeCast.toUint32(srcChainId);

        bytes memory expected = _s().remoteMessenger[chainId];
        require(
            expected.length != 0 && keccak256(sender) == keccak256(expected),
            UnauthorizedSourceMessenger(chainId, sender)
        );

        _dispatch(chainId, receiveId, payload);
        return IERC7786Recipient.receiveMessage.selector;
    }

    /// @dev Handles an authenticated inbound `payload` from the matching messenger on `srcChainId`.
    /// @param srcChainId Source EVM chainId the message was authenticated against.
    /// @param receiveId Bridge-assigned unique message id (binds source bridge + nonce-bearing
    ///        payload). Clients that isolate per-item work (e.g. the NFT bridge clients) can use it
    ///        as an idempotency/parking key.
    /// @param payload Encoded message body delivered verbatim from the remote messenger.
    function _dispatch(uint32 srcChainId, bytes32 receiveId, bytes calldata payload) internal virtual;
}
