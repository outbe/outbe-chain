// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {ReentrancyGuardUpgradeable} from "@openzeppelin/contracts-upgradeable/utils/ReentrancyGuardUpgradeable.sol";
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

import {IERC1155Bridgeable} from "./interfaces/IERC1155Bridgeable.sol";
import {IONFT1155AdapterBatch, BatchSendParam, MultiRecipientSendParam} from "./interfaces/IONFT1155AdapterBatch.sol";
import {LzGasEstimator} from "./libs/LzGasEstimator.sol";
import {ONFT1155BatchMsgCodec} from "./libs/ONFT1155BatchMsgCodec.sol";

/// @dev Constructor-only guard. `_lzEndpoint` is consumed by the `OAppUpgradeable` base constructor
///      before the derived body runs, so a zero value can only be caught from the inheritance
///      argument list — there it surfaces a typed `ZeroAddress` instead of the opaque revert the
///      base would otherwise throw.
function _requireNonZeroAddress(address value, string memory field) pure returns (address) {
    if (value == address(0)) revert IONFT1155AdapterBatch.ZeroAddress(field);
    return value;
}

/**
 * @title ONFT1155AdapterBatch
 * @author Outbe
 * @notice LayerZero OApp adapter for batch cross-chain ERC1155 transfers.
 * @dev UUPS upgradeable: deployed behind an ERC1967 proxy; the LayerZero endpoint and bridged
 *      token stay implementation immutables, so every upgrade passes the same constructor args.
 *      Supports two batch modes:
 *      1. Single recipient, multiple tokens (BatchSendParam)
 *      2. Multiple recipients, each with their own tokens (MultiRecipientSendParam)
 *
 * Benefits:
 * - Pay only ONE LayerZero messaging fee for multiple transfers
 * - Atomic transfer - all tokens transfer together or none do
 * - ~76% cheaper than separate transactions for 5 token types
 */
contract ONFT1155AdapterBatch is
    IONFT1155AdapterBatch,
    OAppUpgradeable,
    OAppOptionsType3Upgradeable,
    AccessControlUpgradeable,
    ReentrancyGuardUpgradeable,
    UUPSUpgradeable
{
    /// @notice Wire `msgType` tag for a single-recipient batch transfer (mirrors the codec constant).
    uint16 public constant SEND = ONFT1155BatchMsgCodec.SEND;
    /// @notice Wire `msgType` tag for a multi-recipient transfer (mirrors the codec constant).
    uint16 public constant SEND_MULTI = ONFT1155BatchMsgCodec.SEND_MULTI;

    /// @notice Active body version emitted by encoders and required by `_lzReceive`.
    /// @dev Wire layout: `[bodyVersion(1)][msgType(1)][abi.encode(payload)]`. Bumped `V1 -> V2`
    ///      with the `abi.encodePacked` -> `abi.encode` migration; a stale V1
    ///      packet now fails closed via `UnsupportedBodyVersion`.
    uint8 public constant BODY_VERSION_V2 = ONFT1155BatchMsgCodec.BODY_VERSION_V2;

    /// @notice Max items per cross-chain batch (unified system-wide cap). Enforced on the
    ///         outbound debit path and the inbound decoded array length (the latter in the codec).
    uint256 public constant MAX_BATCH_SIZE = ONFT1155BatchMsgCodec.MAX_BATCH_SIZE;

    /// @notice Destination gas overhead for an inbound batch independent of item count
    ///         (decode + dispatch + idempotency write + nonReentrant).
    /// @dev Calibrated against a measured `_lzReceive`: 1 item ≈ 247k, 10 ≈ 1.83M, 50 ≈ 8.12M gas
    ///      (cold recipients, worst case). Marginal ≈ 160–176k/item; base picks up the fixed
    ///      overhead. The `LzGasEstimator` 20% buffer absorbs the remainder.
    uint128 internal constant CREDIT_BASE_GAS = 120_000;

    /// @notice Marginal destination gas per credited item: one self-call `creditOne` (call
    ///         overhead) + `token.credit` (ERC-1155 mint + enumerable holder-set bookkeeping +
    ///         supply-cap check).
    uint128 internal constant CREDIT_PER_ITEM_GAS = 180_000;

    /// @notice Granted to TargetMessenger; gates the relay-funded `systemMultiSend` holder migration.
    bytes32 public constant SYSTEM_RELAYER_ROLE = keccak256("SYSTEM_RELAYER_ROLE");

    /// @notice The bridgeable ERC-1155 token this adapter debits on send and credits on receive.
    IERC1155Bridgeable public immutable token;

    /// @notice Snapshot of one item in a batch whose `token.credit` reverted.
    /// @dev `exists` distinguishes "never failed" from "failed and already retried"; on a
    ///      successful retry the slot is deleted so re-retry reverts `NoSuchFailedCredit`.
    struct FailedCredit {
        address to;
        uint256 tokenId;
        uint256 amount;
        bytes reason;
        bool exists;
    }

    /// @custom:storage-location erc7201:outbe.intex.ONFT1155AdapterBatch
    struct ONFT1155AdapterBatchStorage {
        /// @dev Set of inbound `(srcEid, guid)` packets already credited. Batch transfers are
        ///      independent — GUID idempotency (not ORDERED nonce) keeps one delayed batch from
        ///      stalling every other recipient.
        mapping(uint32 srcEid => mapping(bytes32 guid => bool)) processed;
        /// @dev Per-guid map of items in the originating batch whose `token.credit` reverted.
        mapping(bytes32 guid => mapping(uint256 idx => FailedCredit)) failedCredits;
        /// @dev Set only while `systemMultiSend` performs its `_lzSend`, so `_payNative` knows to
        ///      draw the fee from the pre-funded balance. The user-facing `batchSend`/`multiSend`
        ///      leave it false and are always caller-funded — they can never spend the system float.
        bool relayFunded;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.ONFT1155AdapterBatch")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0x6727c2bc99fd63b213294d83fd1690033f4cd7fa6aa4e7cd0cbe5775c7ffda00;

    function _s() private pure returns (ONFT1155AdapterBatchStorage storage $) {
        // solhint-disable-next-line no-inline-assembly
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address _token, address _lzEndpoint)
        OAppUpgradeable(_requireNonZeroAddress(_lzEndpoint, "lzEndpoint"))
    {
        // `token` is immutable: a zero address would permanently brick this adapter.
        if (_token == address(0)) revert ZeroAddress("token");
        token = IERC1155Bridgeable(_token);
        _disableInitializers();
    }

    /// @notice Initializes the proxy: LayerZero delegate, contract owner, and admin role holder.
    /// @param _delegate Owner, endpoint delegate, and receiver of `DEFAULT_ADMIN_ROLE`.
    function initialize(address _delegate) external initializer {
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
    /// @notice Failed credit snapshot for item `idx` of the batch keyed by `guid`.
    /// @param guid Inbound packet GUID.
    /// @param idx Position of the item in that batch.
    /// @return to Recipient address that failed to credit.
    /// @return tokenId Token ID that failed to credit.
    /// @return amount Amount that failed to credit.
    /// @return reason Raw revert data from the failed credit.
    /// @return exists True when a parked failed-credit entry is present.
    function failedCredits(bytes32 guid, uint256 idx)
        external
        view
        returns (address to, uint256 tokenId, uint256 amount, bytes memory reason, bool exists)
    {
        FailedCredit storage f = _s().failedCredits[guid][idx];
        return (f.to, f.tokenId, f.amount, f.reason, f.exists);
    }

    /// @notice ERC-165 support check; reports `IONFT1155AdapterBatch` in addition to inherited interfaces.
    /// @param interfaceId Interface ID to check
    /// @return True if the interface is supported
    function supportsInterface(bytes4 interfaceId) public view override(AccessControlUpgradeable) returns (bool) {
        return interfaceId == type(IONFT1155AdapterBatch).interfaceId || super.supportsInterface(interfaceId);
    }

    // --- Single-recipient batch ---
    /// @inheritdoc IONFT1155AdapterBatch
    function quoteBatchSend(BatchSendParam calldata _sendParam, bool _payInLzToken)
        external
        view
        returns (MessagingFee memory fee)
    {
        (bytes memory message, bytes memory options) = _buildBatchMsgAndOptions(_sendParam);
        return _quote(_sendParam.dstEid, message, options, _payInLzToken);
    }

    /// @inheritdoc IONFT1155AdapterBatch
    function batchSend(BatchSendParam calldata _sendParam, MessagingFee calldata _fee, address _refundAddress)
        external
        payable
        nonReentrant
        returns (MessagingReceipt memory msgReceipt)
    {
        if (_sendParam.tokenIds.length == 0) revert EmptyBatch();
        if (_sendParam.tokenIds.length != _sendParam.amounts.length) revert ArrayLengthMismatch();

        // Build first: the zero-`to` and `MAX_BATCH_SIZE` guards live here, so an invalid or
        // over-size batch fails fast before any `token.debit` runs.
        (bytes memory message, bytes memory options) = _buildBatchMsgAndOptions(_sendParam);

        for (uint256 i = 0; i < _sendParam.tokenIds.length; i++) {
            token.debit(msg.sender, _sendParam.tokenIds[i], _sendParam.amounts[i]);
        }

        msgReceipt = _lzSend(_sendParam.dstEid, message, options, _fee, _refundAddress);

        emit ONFTBatchSent(msgReceipt.guid, _sendParam.dstEid, msg.sender, _sendParam.tokenIds, _sendParam.amounts);
    }

    /// @notice Encode the LZ message and size the receive options for a single-recipient batch.
    /// @dev Body: `abi.encodePacked(BODY_VERSION_V2, SEND, abi.encode(BatchPayload))` — single-pass
    ///      `abi.encode` (no growing-buffer concat). Caps the batch fail-fast so neither the debit
    ///      loop in `batchSend` nor the LZ fee in `quoteBatchSend` runs for an over-size batch.
    /// @param _sendParam Batch send parameters
    /// @return message Encoded LayerZero message
    /// @return options Combined enforced + extra options
    function _buildBatchMsgAndOptions(BatchSendParam calldata _sendParam)
        internal
        pure
        returns (bytes memory message, bytes memory options)
    {
        if (_sendParam.to == bytes32(0)) revert InvalidReceiver();
        if (_sendParam.tokenIds.length > MAX_BATCH_SIZE) {
            revert ONFT1155BatchMsgCodec.BatchTooLarge(_sendParam.tokenIds.length, MAX_BATCH_SIZE);
        }

        message = ONFT1155BatchMsgCodec.encodeBatch(
            ONFT1155BatchMsgCodec.BatchPayload({
                to: _sendParam.to, tokenIds: _sendParam.tokenIds, amounts: _sendParam.amounts
            })
        );

        // Destination credits `tokenIds.length` items in a loop; size the gas option to that count
        // so a large batch does not OOM the inbound `_lzReceive`. The contract owns liveness sizing
        // here — a single dynamic `lzReceiveOption` is the complete options blob (no compose / DVN
        // extras are needed for a plain credit batch), so we skip `combineOptions`.
        options = _receiveOption(_sendParam.tokenIds.length);
    }

    // --- Multi-recipient batch ---
    /// @inheritdoc IONFT1155AdapterBatch
    function quoteMultiSend(MultiRecipientSendParam calldata _sendParam, bool _payInLzToken)
        external
        view
        returns (MessagingFee memory fee)
    {
        (bytes memory message, bytes memory options) = _buildMultiMsgAndOptions(_sendParam);
        return _quote(_sendParam.dstEid, message, options, _payInLzToken);
    }

    /// @inheritdoc IONFT1155AdapterBatch
    function multiSend(MultiRecipientSendParam calldata _sendParam, MessagingFee calldata _fee, address _refundAddress)
        external
        payable
        nonReentrant
        returns (MessagingReceipt memory msgReceipt)
    {
        uint256 len = _sendParam.recipients.length;
        if (len == 0) revert EmptyBatch();
        if (len != _sendParam.tokenIds.length || len != _sendParam.amounts.length) {
            revert ArrayLengthMismatch();
        }

        // Build first so the `MAX_BATCH_SIZE` guard fails fast before any `token.debit`.
        (bytes memory message, bytes memory options) = _buildMultiMsgAndOptions(_sendParam);

        for (uint256 i = 0; i < len; i++) {
            if (_sendParam.recipients[i] == bytes32(0)) revert InvalidReceiver();
            token.debit(msg.sender, _sendParam.tokenIds[i], _sendParam.amounts[i]);
        }

        msgReceipt = _lzSend(_sendParam.dstEid, message, options, _fee, _refundAddress);

        emit ONFTMultiSent(
            msgReceipt.guid,
            _sendParam.dstEid,
            msg.sender,
            _sendParam.recipients,
            _sendParam.tokenIds,
            _sendParam.amounts
        );
    }

    /// @notice Encode the LZ message and size the receive options for a multi-recipient transfer.
    /// @dev Body: `abi.encodePacked(BODY_VERSION_V2, SEND_MULTI, abi.encode(MultiPayload))`.
    /// @param _sendParam Multi-recipient send parameters
    /// @return message Encoded LayerZero message
    /// @return options Combined enforced + extra options
    function _buildMultiMsgAndOptions(MultiRecipientSendParam calldata _sendParam)
        internal
        pure
        returns (bytes memory message, bytes memory options)
    {
        if (_sendParam.recipients.length > MAX_BATCH_SIZE) {
            revert ONFT1155BatchMsgCodec.BatchTooLarge(_sendParam.recipients.length, MAX_BATCH_SIZE);
        }

        message = ONFT1155BatchMsgCodec.encodeMulti(
            ONFT1155BatchMsgCodec.MultiPayload({
                recipients: _sendParam.recipients, tokenIds: _sendParam.tokenIds, amounts: _sendParam.amounts
            })
        );

        // Gas option scales with recipient count (see `_buildBatchMsgAndOptions`).
        options = _receiveOption(_sendParam.recipients.length);
    }

    // --- System bridge ---
    /// @inheritdoc IONFT1155AdapterBatch
    function quoteSystemMultiSend(
        uint256 tokenId,
        address[] calldata holders,
        uint256[] calldata amounts,
        uint32 dstEid,
        bytes calldata extraOptions,
        bool payInLzToken
    ) external view returns (MessagingFee memory fee) {
        bytes memory message = _buildSystemMultiMsg(tokenId, holders, amounts);
        bytes memory options = _receiveOption(holders.length);
        return _quote(dstEid, message, options, payInLzToken);
    }

    /// @inheritdoc IONFT1155AdapterBatch
    function systemMultiSend(
        uint256 tokenId,
        address[] calldata holders,
        uint256[] calldata amounts,
        uint32 dstEid,
        bytes calldata extraOptions,
        MessagingFee calldata fee
    ) external payable onlyRole(SYSTEM_RELAYER_ROLE) nonReentrant returns (MessagingReceipt memory msgReceipt) {
        uint256 len = holders.length;
        if (len == 0) revert EmptyBatch();
        if (len != amounts.length) revert ArrayLengthMismatch();

        // Build first so the `MAX_BATCH_SIZE` guard fails fast before any `token.debit`.
        bytes memory message = _buildSystemMultiMsg(tokenId, holders, amounts);
        bytes memory options = _receiveOption(len);

        for (uint256 i = 0; i < len; i++) {
            token.debit(holders[i], tokenId, amounts[i]);
        }

        // Relay-funded: pay the fee from the pre-funded balance (this runs from TargetMessenger's
        // `_lzReceive` where msg.value is 0). Reset immediately after the send.
        ONFT1155AdapterBatchStorage storage $ = _s();
        $.relayFunded = true;
        msgReceipt = _lzSend(dstEid, message, options, fee, address(this));
        $.relayFunded = false;

        emit SystemMultiSent(msgReceipt.guid, dstEid, tokenId, len);
    }

    // --- Internal helpers ---
    /// @notice Build a SEND_MULTI message where every entry shares the same tokenId.
    /// @param tokenId Token ID (series) shared by all entries
    /// @param holders Source chain holder addresses
    /// @param amounts Corresponding balances for each holder
    /// @return message Encoded LayerZero message
    function _buildSystemMultiMsg(uint256 tokenId, address[] calldata holders, uint256[] calldata amounts)
        internal
        pure
        returns (bytes memory message)
    {
        uint256 len = holders.length;
        if (len > MAX_BATCH_SIZE) revert ONFT1155BatchMsgCodec.BatchTooLarge(len, MAX_BATCH_SIZE);

        bytes32[] memory recipients = new bytes32[](len);
        uint256[] memory tokenIds = new uint256[](len);
        for (uint256 i = 0; i < len; i++) {
            recipients[i] = bytes32(uint256(uint160(holders[i])));
            tokenIds[i] = tokenId;
        }
        message = ONFT1155BatchMsgCodec.encodeMulti(
            ONFT1155BatchMsgCodec.MultiPayload({recipients: recipients, tokenIds: tokenIds, amounts: amounts})
        );
    }

    /// @dev Build the destination `lzReceiveOption` sized for an inbound credit loop of `itemCount`
    ///      items. Keeps the inbound `_lzReceive` from running out of gas on large batches.
    function _receiveOption(uint256 itemCount) internal pure returns (bytes memory) {
        return LzGasEstimator.receiveOption(CREDIT_BASE_GAS, CREDIT_PER_ITEM_GAS, itemCount);
    }

    /// @notice Pay the LayerZero native fee, drawing from the pre-funded balance on the relay path.
    /// @dev When `relayFunded` (set by `systemMultiSend`, which runs from TargetMessenger's
    ///      `_lzReceive` with `msg.value == 0`) the fee is drawn from `address(this).balance`.
    ///      Otherwise the caller funds it via `msg.value` and any excess is refunded to `msg.sender`.
    /// @param _nativeFee Required native fee amount
    /// @return nativeFee Actual fee paid
    function _payNative(uint256 _nativeFee) internal override returns (uint256 nativeFee) {
        if (_s().relayFunded) {
            // Relay path (systemMultiSend): pay from the pre-funded balance, msg.value is 0.
            if (address(this).balance < _nativeFee) revert NotEnoughNative(address(this).balance);
            return _nativeFee;
        }

        // Entry path (batchSend/multiSend): the caller funds the fee; refund any excess so it does
        // not silently seed the system float.
        if (msg.value < _nativeFee) revert MsgValueBelowFee(msg.value, _nativeFee);
        uint256 refund = msg.value - _nativeFee;
        if (refund > 0) {
            // slither-disable-next-line arbitrary-send-eth
            (bool ok,) = msg.sender.call{value: refund}("");
            if (!ok) revert RefundFailed();
        }
        return _nativeFee;
    }

    /// @notice Accept native tokens for LayerZero fees (pre-funding).
    receive() external payable {}

    /// @inheritdoc IONFT1155AdapterBatch
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

    // --- Receive ---
    /// @notice Validate and route an inbound LayerZero message to its credit handler by msgType.
    /// @dev Validation order: idempotency → minimum header length → body version → msgType allowed-set
    ///      → per-handler `abi.decode`. The `_executor` and `_extraData` LZ params are unused here.
    /// @param _origin Source chain origin data
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
        // Idempotency first — a redelivered `(srcEid, guid)` reverts before any state mutation.
        // Shared nonReentrant across all four entrypoints blocks credit-callback re-entry.
        // Validation order: idempotency → minimum header → version → msgType allowed-set → the
        // per-handler `abi.decode` (which reverts on a structurally-bad body and is then
        // length-/size-validated by the codec).
        ONFT1155AdapterBatchStorage storage $ = _s();
        if ($.processed[_origin.srcEid][_guid]) revert AlreadyProcessed(_origin.srcEid, _guid);
        $.processed[_origin.srcEid][_guid] = true;

        if (_message.length < ONFT1155BatchMsgCodec.HEADER_LEN) {
            revert ONFT1155BatchMsgCodec.InvalidPayloadLength(_message.length, ONFT1155BatchMsgCodec.HEADER_LEN);
        }
        uint8 version = uint8(_message[0]);
        if (version != BODY_VERSION_V2) revert ONFT1155BatchMsgCodec.UnsupportedBodyVersion(version);
        uint8 msgType = uint8(_message[1]);

        if (msgType == SEND) {
            _handleBatchReceive(_origin, _guid, _message);
        } else if (msgType == SEND_MULTI) {
            _handleMultiReceive(_origin, _guid, _message);
        } else {
            revert UnknownMsgType(msgType);
        }
    }

    /// @notice Decode and credit a single-recipient batch transfer.
    /// @dev `decodeBatch` validates version + array-length match + `MAX_BATCH_SIZE`; the adapter
    ///      then owns the address semantics: high-bit `MalformedAddress` and the explicit
    ///      `bytes32(0)` reject (reusing `InvalidReceiver`, mirroring the outbound zero-`to` guard).
    /// @param _origin Source chain origin data
    /// @param _guid Unique message identifier
    /// @param _message Encoded batch transfer payload
    function _handleBatchReceive(Origin calldata _origin, bytes32 _guid, bytes calldata _message) internal {
        ONFT1155BatchMsgCodec.BatchPayload memory p = ONFT1155BatchMsgCodec.decodeBatch(_message);

        ONFT1155BatchMsgCodec.assertAddress(p.to);
        if (p.to == bytes32(0)) revert InvalidReceiver();
        address toAddress = address(uint160(uint256(p.to)));

        for (uint256 i = 0; i < p.tokenIds.length; i++) {
            _tryCreditOne(_origin.srcEid, _guid, i, toAddress, p.tokenIds[i], p.amounts[i]);
        }

        emit ONFTBatchReceived(_guid, _origin.srcEid, toAddress, p.tokenIds, p.amounts);
    }

    /// @notice Decode and credit a multi-recipient transfer.
    /// @dev `decodeMulti` validates version + array-length match + `MAX_BATCH_SIZE`; the per-item
    ///      loop owns the address semantics: high-bit `MalformedAddress` and the explicit
    ///      `bytes32(0)` reject (`InvalidReceiver`), so a malformed entry cannot slip past as
    ///      `address(0)`.
    /// @param _origin Source chain origin data
    /// @param _guid Unique message identifier
    /// @param _message Encoded multi-recipient transfer payload
    function _handleMultiReceive(Origin calldata _origin, bytes32 _guid, bytes calldata _message) internal {
        ONFT1155BatchMsgCodec.MultiPayload memory p = ONFT1155BatchMsgCodec.decodeMulti(_message);

        for (uint256 i = 0; i < p.recipients.length; i++) {
            ONFT1155BatchMsgCodec.assertAddress(p.recipients[i]);
            if (p.recipients[i] == bytes32(0)) revert InvalidReceiver();
            address toAddress = address(uint160(uint256(p.recipients[i])));
            _tryCreditOne(_origin.srcEid, _guid, i, toAddress, p.tokenIds[i], p.amounts[i]);
        }

        emit ONFTMultiReceived(_guid, _origin.srcEid, p.recipients, p.tokenIds, p.amounts);
    }

    /// @dev Self-call wrapper around `token.credit` that isolates per-item reverts. A failure
    ///      on item `i` records a `FailedCredit` snapshot for `(guid, i)` and emits `CreditFailed`
    ///      instead of reverting the whole batch — that's the Critical funds-lock fix.
    function _tryCreditOne(uint32 srcEid, bytes32 guid, uint256 idx, address to, uint256 tokenId, uint256 amount)
        internal
    {
        try this.creditOne(to, tokenId, amount) {
        // ok — credit landed
        }
        catch (bytes memory reason) {
            _s().failedCredits[guid][idx] =
                FailedCredit({to: to, tokenId: tokenId, amount: amount, reason: reason, exists: true});
            emit CreditFailed(srcEid, guid, idx, to, tokenId, amount, reason);
        }
    }

    /// @notice Self-call shim around `token.credit`. Only callable by this contract itself —
    ///         exposing it externally would let anyone mint tokens for arbitrary recipients.
    /// @dev `external` (not `public`) so the self-call goes through the EVM call boundary and the
    ///      revert lands in the catch-block of `_tryCreditOne`. The `NotSelf` guard fires the
    ///      moment a non-self caller attempts to use it.
    /// @param to Recipient address to credit
    /// @param tokenId ERC-1155 token id to mint
    /// @param amount Amount to credit
    function creditOne(address to, uint256 tokenId, uint256 amount) external {
        if (msg.sender != address(this)) revert NotSelf();
        token.credit(to, tokenId, amount);
    }

    /// @notice Permissionless retry of a previously-failed credit. On success the entry is
    ///         deleted so a re-retry reverts `NoSuchFailedCredit`. Mirrors the
    ///         `EscrowAdapter.retryFinalize` shape
    /// @param guid Inbound packet GUID where the credit originally failed.
    /// @param idx Position of the failed item in that batch.
    function retryCredit(bytes32 guid, uint256 idx) external nonReentrant {
        ONFT1155AdapterBatchStorage storage $ = _s();
        FailedCredit memory f = $.failedCredits[guid][idx];
        if (!f.exists) revert NoSuchFailedCredit(guid, idx);
        delete $.failedCredits[guid][idx];
        token.credit(f.to, f.tokenId, f.amount);
        emit CreditRetried(guid, idx);
    }
}
