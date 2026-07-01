// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {
    OAppUpgradeable,
    Origin,
    MessagingFee,
    MessagingReceipt
} from "@layerzerolabs/oapp-evm-upgradeable/oapp/OAppUpgradeable.sol";
import {
    OAppOptionsType3Upgradeable
} from "@layerzerolabs/oapp-evm-upgradeable/oapp/libs/OAppOptionsType3Upgradeable.sol";
import {ONFTComposeMsgCodec} from "@layerzerolabs/onft-evm/libs/ONFTComposeMsgCodec.sol";

import {IERC1155Bridgeable} from "./interfaces/IERC1155Bridgeable.sol";
import {IONFT1155Adapter, SendParam} from "./interfaces/IONFT1155Adapter.sol";
import {ONFT1155MsgCodec} from "./libs/ONFT1155MsgCodec.sol";

/**
 * @title ONFT1155Adapter
 * @author Outbe
 * @notice LayerZero OApp adapter for cross-chain ERC1155 transfers.
 * @dev UUPS upgradeable: deployed behind an ERC1967 proxy; the LayerZero endpoint, bridged token,
 *      and peer EID stay implementation immutables, so every upgrade passes the same constructor
 *      args. Token must implement IERC1155Bridgeable and grant access to this adapter.
 */
contract ONFT1155Adapter is
    IONFT1155Adapter,
    OAppUpgradeable,
    OAppOptionsType3Upgradeable,
    ReentrancyGuardTransient,
    UUPSUpgradeable
{
    using ONFT1155MsgCodec for bytes;

    /// @notice LZ message-type tag for a plain transfer (no compose). Selects enforced options.
    uint16 public constant SEND = 1;
    /// @notice LZ message-type tag for a transfer carrying a compose payload. Selects enforced options.
    uint16 public constant SEND_AND_COMPOSE = 2;

    /// @notice The ERC1155 token this adapter crosschainBurns on send and crosschainMints on receive.
    IERC1155Bridgeable public immutable token;

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

    /// @notice Snapshot of an inbound transfer whose `token.crosschainMint` reverted.
    /// @dev `composeMsgData` is the already-encoded compose payload (empty if the transfer carried
    ///      no compose), forwarded after a successful retry. `exists` flips to false on retry so a
    ///      re-retry reverts `NoSuchFailedCrosschainMint`.
    struct FailedCrosschainMint {
        address to;
        uint256 tokenId;
        uint256 amount;
        uint32 srcEid;
        bytes composeMsgData;
        bytes reason;
        bool exists;
    }

    /// @custom:storage-location erc7201:outbe.intex.ONFT1155Adapter
    struct ONFT1155AdapterStorage {
        /// @dev Set of inbound `(srcEid, guid)` packets already crosschainMinted. ONFT transfers are
        ///      independent: GUID idempotency (not ORDERED nonce) keeps one stuck transfer from
        ///      stalling every other transfer on the same channel.
        mapping(uint32 srcEid => mapping(bytes32 guid => bool)) processed;
        /// @dev Inbound compose forwards parked because the original `endpoint.sendCompose` reverted.
        mapping(uint256 idx => PendingCompose) pendingComposes;
        /// @dev Monotonic counter that assigns the next `pendingComposes` slot index.
        uint256 nextPendingComposeIdx;
        /// @dev Inbound transfers whose `token.crosschainMint` reverted, keyed by packet GUID.
        mapping(bytes32 guid => FailedCrosschainMint) failedCrosschainMints;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.ONFT1155Adapter")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0xf7ba3c8714f9cd40d66e510e06f778c613706e48a100857a0ad130fc96ece900;

    function _s() private pure returns (ONFT1155AdapterStorage storage $) {
        // solhint-disable-next-line no-inline-assembly
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @notice Inbound packet with this `(srcEid, guid)` has already been processed.
    error AlreadyProcessed(uint32 srcEid, bytes32 guid);

    /// @notice `deliverCompose` was invoked by an external caller; only `address(this)` is allowed.
    error NotSelf();

    /// @notice `flushPendingCompose` called for an index that was never enqueued.
    error NoSuchPendingCompose(uint256 idx);

    /// @notice `flushPendingCompose` called twice for the same index — the slot was already flushed.
    error AlreadyFlushed(uint256 idx);

    /// @notice No failed-crosschainMint entry exists for `guid`.
    error NoSuchFailedCrosschainMint(bytes32 guid);

    /// @notice Parked entry carries no origin endpoint id (pre-upgrade entry); reclaim cannot route back.
    error NoSourceEid(bytes32 guid);

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

    /// @notice Emitted when `retryCrosschainMint` successfully crosschainMints a previously failed transfer.
    /// @param guid Inbound packet GUID whose parked crosschainMint was retried.
    event CrosschainMintRetried(bytes32 indexed guid);

    /// @notice Emitted when a terminally-failed crosschainMint is reclaimed to its origin chain for re-mint.
    /// @param guid Inbound packet GUID whose parked crosschainMint was reclaimed.
    /// @param srcEid Origin endpoint id the reverse transfer was sent to.
    /// @param to Holder re-minted on the origin chain.
    /// @param tokenId Token ID reclaimed.
    /// @param amount Amount reclaimed.
    event CrosschainMintReclaimed(
        bytes32 indexed guid, uint32 indexed srcEid, address indexed to, uint256 tokenId, uint256 amount
    );

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address _token, address _lzEndpoint) OAppUpgradeable(_lzEndpoint) {
        // `token` is immutable: a zero address would permanently brick this adapter.
        if (_token == address(0)) revert ZeroAddress("token");
        token = IERC1155Bridgeable(_token);
        _disableInitializers();
    }

    /// @notice Initializes the proxy: LayerZero delegate and contract owner.
    /// @param _delegate Owner and endpoint delegate.
    function initialize(address _delegate) external initializer {
        if (_delegate == address(0)) revert ZeroAddress("delegate");
        __Ownable_init(_delegate);
        __OApp_init(_delegate);
    }

    /// @dev Upgrades are gated by the owner.
    /// @param newImplementation Address of the implementation the proxy switches to.
    // solhint-disable-next-line no-empty-blocks
    function _authorizeUpgrade(address newImplementation) internal override onlyOwner {}

    // --- Storage getters ---
    /// @notice Inbound compose forward parked at `idx` because `endpoint.sendCompose` reverted.
    /// @param idx Parked slot index.
    /// @return to Recipient of the deferred compose forward.
    /// @return guid Inbound packet GUID the compose belongs to.
    /// @return composeMsgData Already-encoded compose payload.
    /// @return exists True when the index holds a parked compose.
    /// @return done True when the compose was already flushed.
    function pendingComposes(uint256 idx)
        external
        view
        returns (address to, bytes32 guid, bytes memory composeMsgData, bool exists, bool done)
    {
        PendingCompose storage p = _s().pendingComposes[idx];
        return (p.to, p.guid, p.composeMsgData, p.exists, p.done);
    }

    /// @notice Monotonic counter that assigns the next `pendingComposes` slot index.
    /// @return The next compose slot index.
    function nextPendingComposeIdx() external view returns (uint256) {
        return _s().nextPendingComposeIdx;
    }

    /// @notice Inbound transfer whose `token.crosschainMint` reverted, keyed by packet GUID.
    /// @param guid Inbound packet GUID.
    /// @return to Intended crosschainMint recipient.
    /// @return tokenId Token ID that failed to crosschainMint.
    /// @return amount Amount that failed to crosschainMint.
    /// @return composeMsgData Already-encoded compose payload (empty if none).
    /// @return reason Raw revert data from the failed crosschainMint.
    /// @return exists True when a parked failed-crosschainMint entry is present.
    function failedCrosschainMints(bytes32 guid)
        external
        view
        returns (
            address to,
            uint256 tokenId,
            uint256 amount,
            bytes memory composeMsgData,
            bytes memory reason,
            bool exists
        )
    {
        FailedCrosschainMint storage f = _s().failedCrosschainMints[guid];
        return (f.to, f.tokenId, f.amount, f.composeMsgData, f.reason, f.exists);
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
    /// @notice LayerZero receive handler: crosschainMints tokens on the destination chain and forwards any
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
        ONFT1155AdapterStorage storage $ = _s();
        if ($.processed[_origin.srcEid][_guid]) revert AlreadyProcessed(_origin.srcEid, _guid);
        $.processed[_origin.srcEid][_guid] = true;

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
            $.failedCrosschainMints[_guid] = FailedCrosschainMint({
                to: toAddress,
                tokenId: tokenId_,
                amount: amount_,
                srcEid: _origin.srcEid,
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
    /// @param to CrosschainMint recipient.
    /// @param tokenId Token ID to crosschainMint.
    /// @param amount Amount to crosschainMint.
    function crosschainMintOne(address to, uint256 tokenId, uint256 amount) external {
        if (msg.sender != address(this)) revert NotSelf();
        token.crosschainMint(to, tokenId, amount);
    }

    /// @notice Permissionless retry of a transfer whose inbound crosschainMint failed. Re-crosschainMints the
    ///         tokens and, if the transfer carried a compose, forwards it. The entry is cleared on
    ///         success so a re-retry reverts `NoSuchFailedCrosschainMint`.
    /// @param guid Inbound packet GUID where the crosschainMint originally failed.
    function retryCrosschainMint(bytes32 guid) external nonReentrant {
        ONFT1155AdapterStorage storage $ = _s();
        FailedCrosschainMint memory f = $.failedCrosschainMints[guid];
        if (!f.exists) revert NoSuchFailedCrosschainMint(guid);
        delete $.failedCrosschainMints[guid];

        token.crosschainMint(f.to, f.tokenId, f.amount);
        if (f.composeMsgData.length != 0) {
            _tryDeliverCompose(f.to, guid, f.composeMsgData);
        }
        emit CrosschainMintRetried(guid);
    }

    /// @notice Permissionless reclaim of a crosschainMint the destination gate rejects terminally:
    ///         re-mints the holder on the origin chain via a reverse transfer, the only exit that does
    ///         not re-hit the destination lifecycle gate. Consumes the entry once (CEI delete first).
    /// @param guid Inbound packet GUID whose crosschainMint is stranded.
    /// @param extraOptions Caller-supplied LZ options for the reverse send (combined with enforced options).
    /// @param fee LayerZero messaging fee for the reverse send (caller-funded).
    /// @param refundAddress Address refunded any excess native fee.
    function reclaimToSource(
        bytes32 guid,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable nonReentrant returns (MessagingReceipt memory msgReceipt) {
        ONFT1155AdapterStorage storage $ = _s();
        FailedCrosschainMint memory f = $.failedCrosschainMints[guid];
        if (!f.exists) revert NoSuchFailedCrosschainMint(guid);
        if (f.srcEid == 0) revert NoSourceEid(guid);
        delete $.failedCrosschainMints[guid];

        (bytes memory message,) = ONFT1155MsgCodec.encode(bytes32(uint256(uint160(f.to))), f.tokenId, f.amount, "");
        bytes memory options = combineOptions(f.srcEid, SEND, extraOptions);
        emit CrosschainMintReclaimed(guid, f.srcEid, f.to, f.tokenId, f.amount);
        msgReceipt = _lzSend(f.srcEid, message, options, fee, refundAddress);
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
            ONFT1155AdapterStorage storage $ = _s();
            uint256 idx = $.nextPendingComposeIdx++;
            $.pendingComposes[idx] =
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
        PendingCompose storage p = _s().pendingComposes[idx];
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
