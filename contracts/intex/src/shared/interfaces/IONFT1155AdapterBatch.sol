// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {MessagingFee, MessagingReceipt} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";

/// @notice Parameters for sending a batch of tokens to a SINGLE recipient.
struct BatchSendParam {
    uint32 dstEid;
    bytes32 to;
    uint256[] tokenIds;
    uint256[] amounts;
    bytes extraOptions;
}

/// @notice Parameters for sending tokens to MULTIPLE recipients in one transaction.
/// @dev Each recipient can receive different token IDs and amounts.
struct MultiRecipientSendParam {
    uint32 dstEid;
    bytes32[] recipients;
    uint256[] tokenIds;
    uint256[] amounts;
    bytes extraOptions;
}

/// @title IONFT1155AdapterBatch
/// @author Outbe
/// @notice Interface for batch cross-chain ERC1155 transfers via LayerZero.
/// @dev Supports single-recipient batch and multi-recipient modes.
///      Also provides system bridge functionality for automated holder migration.
interface IONFT1155AdapterBatch {
    // --- Events ---
    /// @notice Emitted when a batch of tokens is sent to one recipient.
    /// @param guid LayerZero message GUID
    /// @param dstEid Destination endpoint ID
    /// @param from Sender address
    /// @param tokenIds Array of token IDs sent
    /// @param amounts Corresponding amounts for each token ID
    event ONFTBatchSent(
        bytes32 indexed guid, uint32 dstEid, address indexed from, uint256[] tokenIds, uint256[] amounts
    );

    /// @notice Emitted when tokens are sent to multiple recipients.
    /// @param guid LayerZero message GUID
    /// @param dstEid Destination endpoint ID
    /// @param from Sender address
    /// @param recipients Array of recipient addresses (bytes32-encoded)
    /// @param tokenIds Array of token IDs sent
    /// @param amounts Corresponding amounts for each recipient
    event ONFTMultiSent(
        bytes32 indexed guid,
        uint32 dstEid,
        address indexed from,
        bytes32[] recipients,
        uint256[] tokenIds,
        uint256[] amounts
    );

    /// @notice Emitted when a batch of tokens is received for one recipient.
    /// @param guid LayerZero message GUID
    /// @param srcEid Source endpoint ID
    /// @param to Recipient address
    /// @param tokenIds Array of token IDs received
    /// @param amounts Corresponding amounts for each token ID
    event ONFTBatchReceived(
        bytes32 indexed guid, uint32 srcEid, address indexed to, uint256[] tokenIds, uint256[] amounts
    );

    /// @notice Emitted when tokens are received for multiple recipients.
    /// @param guid LayerZero message GUID
    /// @param srcEid Source endpoint ID
    /// @param recipients Array of recipient addresses (bytes32-encoded)
    /// @param tokenIds Array of token IDs received
    /// @param amounts Corresponding amounts for each recipient
    event ONFTMultiReceived(
        bytes32 indexed guid, uint32 srcEid, bytes32[] recipients, uint256[] tokenIds, uint256[] amounts
    );

    /// @notice Emitted when system bridge migrates holders cross-chain.
    /// @param guid LayerZero message GUID
    /// @param dstEid Destination endpoint ID
    /// @param tokenId Token ID (series) being migrated
    /// @param holdersCount Number of holders included in the migration
    event SystemMultiSent(bytes32 indexed guid, uint32 dstEid, uint256 indexed tokenId, uint256 holdersCount);

    /// @notice Emitted when a holder system-bridge's `systemMultiSend` reverts and the snapshot is parked.
    /// @param idx Index of the parked `pendingHoldersBridges` slot.
    /// @param tokenId Token id (series) whose holder bridge was deferred.
    /// @param holdersCount Number of holders in the deferred bridge.
    /// @param reason Raw revert data from the failed send.
    event HoldersBridgeDeferred(uint256 indexed idx, uint256 indexed tokenId, uint256 holdersCount, bytes reason);

    /// @notice Emitted when `flushHoldersBridge` successfully retries a previously deferred bridge.
    /// @param idx Index of the flushed `pendingHoldersBridges` slot.
    /// @param tokenId Token id (series) whose holders were bridged.
    event HoldersBridgeFlushed(uint256 indexed idx, uint256 indexed tokenId);

    /// @notice Emitted when residual pre-funded native tokens are swept to an admin recipient.
    /// @param to Recipient address the native tokens were swept to
    /// @param amount Amount in wei swept
    event NativeSwept(address indexed to, uint256 amount);

    /// @notice Emitted when one item in an inbound batch reverts `token.crosschainMint`.
    /// @param srcEid LayerZero source endpoint id (`_origin.srcEid`)
    /// @param guid Inbound LayerZero packet GUID
    /// @param idx Position of the failed item in the original batch
    /// @param to Recipient address
    /// @param tokenId ERC-1155 token id
    /// @param amount Amount that failed to crosschainMint
    /// @param reason Raw revert bytes from `token.crosschainMint`
    event CrosschainMintFailed(
        uint32 indexed srcEid,
        bytes32 indexed guid,
        uint256 idx,
        address indexed to,
        uint256 tokenId,
        uint256 amount,
        bytes reason
    );

    /// @notice Emitted when `retryCrosschainMint` successfully mints a previously failed item.
    /// @param guid Inbound LayerZero packet GUID where the crosschainMint originally failed
    /// @param idx Position of the retried item in the original batch
    event CrosschainMintRetried(bytes32 indexed guid, uint256 indexed idx);

    /// @notice Emitted when a terminally-failed item is reclaimed to its origin chain for re-mint.
    /// @param guid Inbound LayerZero packet GUID whose item was reclaimed
    /// @param idx Position of the reclaimed item in the original batch
    /// @param srcEid Origin endpoint id the reverse transfer was sent to
    /// @param to Holder re-minted on the origin chain
    /// @param tokenId Token ID reclaimed
    /// @param amount Amount reclaimed
    event CrosschainMintReclaimed(
        bytes32 indexed guid, uint256 indexed idx, uint32 indexed srcEid, address to, uint256 tokenId, uint256 amount
    );

    // --- Errors ---
    /// @notice Receiver address is zero.
    error InvalidReceiver();
    /// @notice Array lengths do not match.
    error ArrayLengthMismatch();
    /// @notice Empty batch provided.
    error EmptyBatch();
    /// @notice Native-token sweep transfer failed.
    error NativeSweepFailed();

    /// @notice A caller-funded send supplied less native value than the quoted LZ fee.
    error MsgValueBelowFee(uint256 msgValue, uint256 nativeFee);

    /// @notice Refund of excess native value to the caller failed.
    error RefundFailed();
    /// @notice Native-token balance is insufficient for the requested sweep.
    /// @param available Current contract balance
    /// @param requested Amount the admin attempted to sweep
    error NativeBalanceInsufficient(uint256 available, uint256 requested);
    /// @notice Zero address provided.
    /// @param field Field name
    error ZeroAddress(string field);
    /// @notice Inbound `msgType` is not in the SEND / SEND_MULTI set.
    /// @dev Wire-format reverts (`UnsupportedBodyVersion`, `MalformedAddress`, `BatchTooLarge`,
    ///      `InvalidPayloadLength`, `ArrayLengthMismatch`) are owned by `ONFT1155BatchMsgCodec`.
    /// @param got The unsupported message-type tag received.
    error UnknownMsgType(uint8 got);
    /// @notice Inbound packet with this `(srcEid, guid)` has already been minted.
    /// @param srcEid LayerZero source endpoint id of the redelivered packet.
    /// @param guid Inbound LayerZero packet GUID that was already processed.
    error AlreadyProcessed(uint32 srcEid, bytes32 guid);
    /// @notice `crosschainMintOne` was invoked by an external caller; only `address(this)` is allowed.
    /// @dev `crosschainMintOne` is a self-call shim used by `_lzReceive` to isolate per-item `token.crosschainMint`
    ///      reverts. Exposing it externally would let anyone mint tokens for arbitrary recipients.
    error NotSelf();
    /// @notice No failed-crosschainMint entry exists for `(guid, idx)`.
    /// @param guid Inbound LayerZero packet GUID being retried.
    /// @param idx Position in the original batch with no parked failed-crosschainMint slot.
    error NoSuchFailedCrosschainMint(bytes32 guid, uint256 idx);
    /// @notice `flushHoldersBridge` called for an index that holds no parked bridge.
    /// @param idx Enqueue index with no parked holder bridge.
    error NoSuchPendingHoldersBridge(uint256 idx);
    /// @notice Parked entry carries no origin endpoint id (pre-upgrade entry); reclaim cannot route back.
    /// @param guid Inbound LayerZero packet GUID being reclaimed.
    /// @param idx Position in the original batch.
    error NoSourceEid(bytes32 guid, uint256 idx);

    /// @notice Sweep residual pre-funded native tokens back to an admin recipient.
    /// @param to Recipient address (must be non-zero)
    /// @param amount Amount in wei to sweep; must be ≤ contract balance
    function sweepNative(address payable to, uint256 amount) external;

    // --- Single-recipient batch ---
    /// @notice Quotes the messaging fee for a batch cross-chain transfer.
    /// @param _sendParam Batch send parameters
    /// @param _payInLzToken Whether to pay in LZ token
    /// @return fee Messaging fee quote
    function quoteBatchSend(BatchSendParam calldata _sendParam, bool _payInLzToken)
        external
        view
        returns (MessagingFee memory fee);

    /// @notice Sends multiple token types to one recipient on another chain.
    /// @param _sendParam Batch send parameters
    /// @param _fee Messaging fee
    /// @param _refundAddress Address for fee refund
    /// @return msgReceipt Messaging receipt
    function batchSend(BatchSendParam calldata _sendParam, MessagingFee calldata _fee, address _refundAddress)
        external
        payable
        returns (MessagingReceipt memory msgReceipt);

    // --- Multi-recipient ---
    /// @notice Quotes the messaging fee for a multi-recipient cross-chain transfer.
    /// @param _sendParam Multi-recipient send parameters
    /// @param _payInLzToken Whether to pay in LZ token
    /// @return fee Messaging fee quote
    function quoteMultiSend(MultiRecipientSendParam calldata _sendParam, bool _payInLzToken)
        external
        view
        returns (MessagingFee memory fee);

    /// @notice Sends tokens to multiple recipients on another chain.
    /// @param _sendParam Multi-recipient send parameters
    /// @param _fee Messaging fee
    /// @param _refundAddress Address for fee refund
    /// @return msgReceipt Messaging receipt
    function multiSend(MultiRecipientSendParam calldata _sendParam, MessagingFee calldata _fee, address _refundAddress)
        external
        payable
        returns (MessagingReceipt memory msgReceipt);

    // --- System bridge (markCalled → holder migration) ---
    /// @notice Quotes the messaging fee for a system bridge multi-send.
    /// @param tokenId Token ID (series) to bridge
    /// @param holders Holder addresses on source chain
    /// @param amounts Corresponding balances for each holder
    /// @param dstEid Destination endpoint ID
    /// @param extraOptions Additional LayerZero options
    /// @param payInLzToken Whether to pay in LZ token
    /// @return fee Messaging fee quote
    function quoteSystemMultiSend(
        uint256 tokenId,
        address[] calldata holders,
        uint256[] calldata amounts,
        uint32 dstEid,
        bytes calldata extraOptions,
        bool payInLzToken
    ) external view returns (MessagingFee memory fee);

    /// @notice Burns tokens from all holders and sends a single SEND_MULTI LZ message.
    /// @dev Only callable by SYSTEM_RELAYER_ROLE (TargetMessenger).
    /// @param tokenId Token ID (series) to bridge
    /// @param holders Holder addresses on source chain
    /// @param amounts Corresponding balances for each holder
    /// @param dstEid Destination endpoint ID
    /// @param extraOptions Additional LayerZero options
    /// @param fee Messaging fee (caller quotes via quoteSystemMultiSend)
    /// @return msgReceipt Messaging receipt
    function systemMultiSend(
        uint256 tokenId,
        address[] calldata holders,
        uint256[] calldata amounts,
        uint32 dstEid,
        bytes calldata extraOptions,
        MessagingFee calldata fee
    ) external payable returns (MessagingReceipt memory msgReceipt);

    /// @notice Read the series holders off the bridged IntexNFT1155, burn them, and relay-funded-bridge
    ///         them to `dstEid`, parking the snapshot for retry if the send reverts. Called by
    ///         TargetMessenger on an inbound markCalled.
    /// @param seriesId Auction series whose holders migrate.
    /// @param dstEid Destination endpoint id (Outbe).
    function bridgeHoldersWithRecovery(uint32 seriesId, uint32 dstEid) external;

    /// @notice Permissionless retry of a previously deferred holder system-bridge.
    /// @param idx Index of the parked bridge to flush.
    function flushHoldersBridge(uint256 idx) external;

    /// @notice Parked holder system-bridge by enqueue index (scalar fields; arrays stay internal).
    /// @param idx Enqueue index.
    /// @return tokenId Token id whose holder bridge was deferred.
    /// @return dstEid Destination endpoint id the bridge targets.
    /// @return exists True when the index holds a parked bridge.
    function pendingHoldersBridges(uint256 idx) external view returns (uint256 tokenId, uint32 dstEid, bool exists);

    /// @notice Next index to assign in `pendingHoldersBridges`; also the count ever enqueued.
    /// @return The next enqueue index.
    function nextPendingHoldersBridgeIdx() external view returns (uint256);
}
