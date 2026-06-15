// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import {OApp, Origin, MessagingFee, MessagingReceipt} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";
import {OAppOptionsType3} from "@layerzerolabs/oapp-evm/oapp/libs/OAppOptionsType3.sol";
import {ONFTComposeMsgCodec} from "@layerzerolabs/onft-evm/libs/ONFTComposeMsgCodec.sol";

import {IERC1155Bridgeable} from "./interfaces/IERC1155Bridgeable.sol";
import {IONFT1155Adapter, SendParam} from "./interfaces/IONFT1155Adapter.sol";
import {ONFT1155MsgCodec} from "./libs/ONFT1155MsgCodec.sol";

/**
 * @title ONFT1155Adapter
 * @author Outbe
 * @notice LayerZero OApp adapter for cross-chain ERC1155 transfers.
 * @dev Token must implement IERC1155Bridgeable and grant access to this adapter.
 */
contract ONFT1155Adapter is IONFT1155Adapter, OApp, OAppOptionsType3, ReentrancyGuard {
    using ONFT1155MsgCodec for bytes;

    /// @notice LZ message-type tag for a plain transfer (no compose). Selects enforced options.
    uint16 public constant SEND = 1;
    /// @notice LZ message-type tag for a transfer carrying a compose payload. Selects enforced options.
    uint16 public constant SEND_AND_COMPOSE = 2;

    /// @notice The ERC1155 token this adapter burns on send and mints on receive.
    IERC1155Bridgeable public immutable token;
    /// @notice LayerZero endpoint ID of the Outbe chain.
    uint32 public immutable OUTBE_EID;

    /// @notice Set of inbound `(srcEid, guid)` packets already minted.
    /// @dev ONFT transfers are independent: the auction-stage ORDERED guarantee used by
    ///      `TargetMessenger` / `OriginMessenger` is overkill here (one stuck transfer would
    ///      stall every other transfer on the same `(srcEid, sender)` channel). Instead, this
    ///      mapping pins each delivered packet by GUID, and the first action of `_lzReceive`
    ///      asserts then sets the flag so a redelivered packet reverts `AlreadyProcessed` before
    ///      any `token.crosschainMint` runs.
    mapping(uint32 srcEid => mapping(bytes32 guid => bool)) internal processed;

    /// @notice Snapshot of an inbound compose forward whose `endpoint.sendCompose` reverted.
    /// @dev `done` distinguishes "still pending" from "already flushed"; on flush the slot is
    ///      marked done so a re-flush reverts `AlreadyFlushed`.
    struct PendingCompose {
        address to;
        bytes32 guid;
        bytes composeMsgData;
        bool exists;
        bool done;
    }

    /// @notice Inbound compose forwards parked because the original `endpoint.sendCompose` reverted.
    /// @dev `token.crosschainMint` has already landed by the time we attempt the compose forward, so a
    ///      revert there would otherwise force the whole `_lzReceive` to revert and burn the
    ///      already-minted tokens. Pattern A from defer the send, recover via
    ///      `flushPendingCompose` once the composer side is fixed.
    mapping(uint256 idx => PendingCompose) public pendingComposes;
    /// @notice Monotonic counter that assigns the next `pendingComposes` slot index.
    uint256 public nextPendingComposeIdx;

    /// @notice Inbound packet with this `(srcEid, guid)` has already been processed.
    error AlreadyProcessed(uint32 srcEid, bytes32 guid);

    /// @notice `deliverCompose` was invoked by an external caller; only `address(this)` is allowed.
    error NotSelf();

    /// @notice `flushPendingCompose` called for an index that was never enqueued.
    error NoSuchPendingCompose(uint256 idx);

    /// @notice `flushPendingCompose` called twice for the same index — the slot was already flushed.
    error AlreadyFlushed(uint256 idx);

    /// @notice Emitted when `endpoint.sendCompose` reverts inside `_lzReceive` and the compose
    ///         forward is parked for later recovery via `flushPendingCompose`.
    /// @param idx Index of the parked `pendingComposes` slot.
    /// @param guid Inbound packet GUID the compose belongs to.
    /// @param to Recipient of the deferred compose forward.
    /// @param reason Raw revert data returned by the failed `endpoint.sendCompose`.
    event ComposeDeferred(uint256 indexed idx, bytes32 indexed guid, address indexed to, bytes reason);

    /// @notice Emitted when `flushPendingCompose` successfully forwards a previously deferred compose.
    /// @param idx Index of the flushed `pendingComposes` slot.
    event ComposeFlushed(uint256 indexed idx);

    /// @notice Snapshot of an inbound transfer whose `token.crosschainMint` reverted.
    /// @dev `composeMsgData` is the already-encoded compose payload (empty if the transfer carried
    ///      no compose), forwarded after a successful retry. `exists` flips to false on retry so a
    ///      re-retry reverts `NoSuchFailedCrosschainMint`.
    struct FailedCrosschainMint {
        address to;
        uint256 tokenId;
        uint256 amount;
        bytes composeMsgData;
        bytes reason;
        bool exists;
    }

    /// @notice Inbound transfers whose `token.crosschainMint` reverted, keyed by packet GUID.
    /// @dev Without this an inbound crosschainMint revert (e.g. a destination series past its settlement
    ///      deadline) would unwind the whole `_lzReceive` after the sender already burned, stranding
    ///      the tokens. The self-call shim isolates the crosschainMint; on revert the snapshot is parked here
    ///      and `retryCrosschainMint` re-attempts once the upstream cause is cleared.
    mapping(bytes32 guid => FailedCrosschainMint) public failedCrosschainMints;

    /// @notice No failed-crosschainMint entry exists for `guid`.
    error NoSuchFailedCrosschainMint(bytes32 guid);

    /// @notice Emitted when an inbound transfer's `token.crosschainMint` reverts and is parked for retry.
    /// @param srcEid Source endpoint ID the packet arrived from.
    /// @param guid Inbound packet GUID keying the parked `failedCrosschainMints` entry.
    /// @param to Intended crosschainMint recipient.
    /// @param tokenId Token ID that failed to crosschainMint.
    /// @param amount Amount that failed to crosschainMint.
    /// @param reason Raw revert data returned by the failed `token.crosschainMint`.
    event CrosschainMintFailed(
        uint32 indexed srcEid, bytes32 indexed guid, address indexed to, uint256 tokenId, uint256 amount, bytes reason
    );

    /// @notice Emitted when `retryCrosschainMint` successfully mints a previously failed transfer.
    /// @param guid Inbound packet GUID whose parked crosschainMint was retried.
    event CrosschainMintRetried(bytes32 indexed guid);

    constructor(address _token, address _lzEndpoint, address _delegate, uint32 _outbeEid)
        OApp(_lzEndpoint, _delegate)
        Ownable(_delegate)
    {
        // `token` is immutable: a zero address would permanently brick this adapter. `_lzEndpoint`
        // and `_delegate` are already enforced by the OApp/Ownable base constructors: a zero
        // delegate/owner reverts `OwnableInvalidOwner(address(0))` (Ownable linearizes ahead of
        // OAppCore), so only `_token` is unchecked.
        if (_token == address(0)) revert ZeroAddress("token");
        token = IERC1155Bridgeable(_token);
        OUTBE_EID = _outbeEid;
    }

    // --- Send ---
    /// @inheritdoc IONFT1155Adapter
    function quoteSend(SendParam calldata _sendParam, bool _payInLzToken) external view returns (MessagingFee memory) {
        (bytes memory message, bytes memory options) = _buildMsgAndOptions(_sendParam);
        return _quote(_sendParam.dstEid, message, options, _payInLzToken);
    }

    /// @inheritdoc IONFT1155Adapter
    function send(SendParam calldata _sendParam, MessagingFee calldata _fee, address _refundAddress)
        external
        payable
        returns (MessagingReceipt memory msgReceipt)
    {
        token.crosschainBurn(msg.sender, _sendParam.tokenId, _sendParam.amount);

        (bytes memory message, bytes memory options) = _buildMsgAndOptions(_sendParam);

        msgReceipt = _lzSend(_sendParam.dstEid, message, options, _fee, _refundAddress);
        emit ONFTSent(msgReceipt.guid, _sendParam.dstEid, msg.sender, _sendParam.tokenId, _sendParam.amount);
    }

    // --- Internal helpers ---
    /// @notice Encode the LZ message and combine options for a single-token transfer.
    /// @dev Reverts `InvalidReceiver` when `_sendParam.to` is zero. The compose presence reported
    ///      by the codec selects the `SEND` vs `SEND_AND_COMPOSE` enforced-options bucket.
    /// @param _sendParam Transfer parameters (destination, tokenId, amount, options)
    /// @return message Encoded LayerZero message
    /// @return options Combined enforced + extra options
    function _buildMsgAndOptions(SendParam calldata _sendParam)
        internal
        view
        returns (bytes memory message, bytes memory options)
    {
        if (_sendParam.to == bytes32(0)) revert InvalidReceiver();

        bool hasCompose;
        (message, hasCompose) =
            ONFT1155MsgCodec.encode(_sendParam.to, _sendParam.tokenId, _sendParam.amount, _sendParam.composeMsg);

        uint16 msgType = hasCompose ? SEND_AND_COMPOSE : SEND;
        options = combineOptions(_sendParam.dstEid, msgType, _sendParam.extraOptions);
    }

    // --- Receive ---
    /// @notice LayerZero receive handler: mints tokens on the destination chain and forwards any
    ///         compose message, with redelivery guarded by the `processed` set.
    /// @dev Validation order: minimum length (covers bodyVersion + sendTo + tokenId + amount)
    ///      → version assertion (inside codec helpers) → address-bit check on `sendTo`. A
    ///      malformed packet reverts with a typed error before any `token.crosschainMint` runs. The
    ///      trailing `_executor` and `_extraData` parameters are unused and left unnamed.
    /// @param _origin Source chain origin data (srcEid, sender, nonce)
    /// @param _guid Unique message identifier
    /// @param _message Encoded transfer payload
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
        if (processed[_origin.srcEid][_guid]) revert AlreadyProcessed(_origin.srcEid, _guid);
        processed[_origin.srcEid][_guid] = true;

        ONFT1155MsgCodec.assertMinLength(_message);
        bytes32 sendToRaw = _message.sendTo();
        ONFT1155MsgCodec.assertAddress(sendToRaw);
        address toAddress = ONFT1155MsgCodec.bytes32ToAddress(sendToRaw);
        uint256 tokenId_ = _message.tokenId();
        uint256 amount_ = _message.amount();

        bytes memory composeMsgData = _message.isComposed()
            ? ONFTComposeMsgCodec.encode(_origin.nonce, _origin.srcEid, _message.composeMsg())
            : bytes("");

        // Isolate the crosschainMint: a revert (e.g. a destination series past its settlement deadline)
        // parks the transfer for retry instead of unwinding the packet and stranding burned tokens.
        // The compose forward only runs once the tokens actually landed.
        try this.crosschainMintOne(toAddress, tokenId_, amount_) {
            if (composeMsgData.length != 0) {
                _tryDeliverCompose(toAddress, _guid, composeMsgData);
            }
            emit ONFTReceived(_guid, _origin.srcEid, toAddress, tokenId_, amount_);
        } catch (bytes memory reason) {
            failedCrosschainMints[_guid] = FailedCrosschainMint({
                to: toAddress,
                tokenId: tokenId_,
                amount: amount_,
                composeMsgData: composeMsgData,
                reason: reason,
                exists: true
            });
            emit CrosschainMintFailed(_origin.srcEid, _guid, toAddress, tokenId_, amount_, reason);
        }
    }

    /// @notice Self-call shim around `token.crosschainMint`. Only callable by this contract itself, so the
    ///         revert lands in the catch-block of `_lzReceive` and the crosschainMint can be isolated.
    /// @dev Reverts `NotSelf` for any caller other than `address(this)`.
    /// @param to crosschainMint recipient.
    /// @param tokenId Token ID to crosschainMint.
    /// @param amount Amount to crosschainMint.
    function crosschainMintOne(address to, uint256 tokenId, uint256 amount) external {
        if (msg.sender != address(this)) revert NotSelf();
        token.crosschainMint(to, tokenId, amount);
    }

    /// @notice Permissionless retry of a transfer whose inbound crosschainMint failed. Re-mints the
    ///         tokens and, if the transfer carried a compose, forwards it. The entry is cleared on
    ///         success so a re-retry reverts `NoSuchFailedCrosschainMint`.
    /// @param guid Inbound packet GUID where the crosschainMint originally failed.
    function retryCrosschainMint(bytes32 guid) external nonReentrant {
        FailedCrosschainMint memory f = failedCrosschainMints[guid];
        if (!f.exists) revert NoSuchFailedCrosschainMint(guid);
        delete failedCrosschainMints[guid];

        token.crosschainMint(f.to, f.tokenId, f.amount);
        if (f.composeMsgData.length != 0) {
            _tryDeliverCompose(f.to, guid, f.composeMsgData);
        }
        emit CrosschainMintRetried(guid);
    }

    /// @dev Self-call wrapper around `endpoint.sendCompose` that isolates a composer-side revert
    ///      from the inbound packet. `token.crosschainMint` already landed by this point — letting the
    ///      compose forward fail loudly would unwind that effect too. Pattern A: park the request
    ///      and let anyone retry via `flushPendingCompose(idx)`.
    function _tryDeliverCompose(address to, bytes32 guid, bytes memory composeMsgData) internal {
        try this.deliverCompose(to, guid, composeMsgData) {
        // ok — compose forwarded
        }
        catch (bytes memory reason) {
            uint256 idx = nextPendingComposeIdx++;
            pendingComposes[idx] =
                PendingCompose({to: to, guid: guid, composeMsgData: composeMsgData, exists: true, done: false});
            emit ComposeDeferred(idx, guid, to, reason);
        }
    }

    /// @notice Self-call shim around `endpoint.sendCompose`. Only callable by this contract itself —
    ///         exposing it externally would let anyone issue forwarded composes on the adapter's behalf.
    /// @dev `external` (not `internal`) so the self-call goes through the EVM boundary and the revert
    ///      lands in the catch-block of `_tryDeliverCompose`. Reverts `NotSelf` for any external caller.
    /// @param to Compose-message recipient.
    /// @param guid Inbound packet GUID the compose belongs to.
    /// @param composeMsgData Already-encoded compose payload forwarded to `endpoint.sendCompose`.
    function deliverCompose(address to, bytes32 guid, bytes calldata composeMsgData) external {
        if (msg.sender != address(this)) revert NotSelf();
        endpoint.sendCompose(to, guid, 0, composeMsgData);
    }

    /// @notice Permissionless retry of a previously deferred compose forward. Marks the slot done
    ///         on success so a re-flush reverts `AlreadyFlushed`.
    /// @param idx Index of the deferred compose to flush.
    function flushPendingCompose(uint256 idx) external nonReentrant {
        PendingCompose storage p = pendingComposes[idx];
        if (!p.exists) revert NoSuchPendingCompose(idx);
        if (p.done) revert AlreadyFlushed(idx);
        p.done = true;
        endpoint.sendCompose(p.to, p.guid, 0, p.composeMsgData);
        emit ComposeFlushed(idx);
    }

    /// @inheritdoc IONFT1155Adapter
    function sweepNative(address payable to, uint256 amount) external onlyOwner {
        if (to == address(0)) revert ZeroAddress("to");
        uint256 balance = address(this).balance;
        if (amount > balance) revert NativeBalanceInsufficient(balance, amount);

        (bool ok,) = to.call{value: amount}("");
        if (!ok) revert NativeSweepFailed();
        emit NativeSwept(to, amount);
    }
}

