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

    /// @notice Emitted when residual pre-funded native tokens are swept to an admin recipient.
    /// @param to Recipient address the native tokens were swept to
    /// @param amount Amount in wei swept
    event NativeSwept(address indexed to, uint256 amount);

    /// @notice Emitted when one item in an inbound batch reverts `token.credit`.
    /// @param srcEid LayerZero source endpoint id (`_origin.srcEid`)
    /// @param guid Inbound LayerZero packet GUID
    /// @param idx Position of the failed item in the original batch
    /// @param to Recipient address
    /// @param tokenId ERC-1155 token id
    /// @param amount Amount that failed to credit
    /// @param reason Raw revert bytes from `token.credit`
    event CreditFailed(
        uint32 indexed srcEid,
        bytes32 indexed guid,
        uint256 idx,
        address indexed to,
        uint256 tokenId,
        uint256 amount,
        bytes reason
    );

    /// @notice Emitted when `retryCredit` successfully credits a previously failed item.
    /// @param guid Inbound LayerZero packet GUID where the credit originally failed
    /// @param idx Position of the retried item in the original batch
    event CreditRetried(bytes32 indexed guid, uint256 indexed idx);

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
    /// @notice Inbound packet with this `(srcEid, guid)` has already been credited.
    /// @param srcEid LayerZero source endpoint id of the redelivered packet.
    /// @param guid Inbound LayerZero packet GUID that was already processed.
    error AlreadyProcessed(uint32 srcEid, bytes32 guid);
    /// @notice `creditOne` was invoked by an external caller; only `address(this)` is allowed.
    /// @dev `creditOne` is a self-call shim used by `_lzReceive` to isolate per-item `token.credit`
    ///      reverts. Exposing it externally would let anyone mint tokens for arbitrary recipients.
    error NotSelf();
    /// @notice No failed-credit entry exists for `(guid, idx)`.
    /// @param guid Inbound LayerZero packet GUID being retried.
    /// @param idx Position in the original batch with no parked failed-credit slot.
    error NoSuchFailedCredit(bytes32 guid, uint256 idx);

    /// @notice Sweep residual pre-funded native tokens back to an admin recipient.
    /// @param to Recipient address (must be non-zero)
    /// @param amount Amount in wei to sweep; must be ≤ contract balance
    function sweepNative(address payable to, uint256 amount) external;

    // --- Single-recipient batch ---
    /// @notice Quotes the messaging fee for a batch cross-chain transfer.
    /// @param _sendParam Batch send parameters
    /// @param _payInLzToken Whether to pay in LZ token
    /// @return fee Messaging fee quote
    function quoteBatchSend(
        BatchSendParam calldata _sendParam,
        bool _payInLzToken
    ) external view returns (MessagingFee memory fee);

    /// @notice Sends multiple token types to one recipient on another chain.
    /// @param _sendParam Batch send parameters
    /// @param _fee Messaging fee
    /// @param _refundAddress Address for fee refund
    /// @return msgReceipt Messaging receipt
    function batchSend(
        BatchSendParam calldata _sendParam,
        MessagingFee calldata _fee,
        address _refundAddress
    ) external payable returns (MessagingReceipt memory msgReceipt);

    // --- Multi-recipient ---
    /// @notice Quotes the messaging fee for a multi-recipient cross-chain transfer.
    /// @param _sendParam Multi-recipient send parameters
    /// @param _payInLzToken Whether to pay in LZ token
    /// @return fee Messaging fee quote
    function quoteMultiSend(
        MultiRecipientSendParam calldata _sendParam,
        bool _payInLzToken
    ) external view returns (MessagingFee memory fee);

    /// @notice Sends tokens to multiple recipients on another chain.
    /// @param _sendParam Multi-recipient send parameters
    /// @param _fee Messaging fee
    /// @param _refundAddress Address for fee refund
    /// @return msgReceipt Messaging receipt
    function multiSend(
        MultiRecipientSendParam calldata _sendParam,
        MessagingFee calldata _fee,
        address _refundAddress
    ) external payable returns (MessagingReceipt memory msgReceipt);

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

    /// @notice Debits tokens from all holders and sends a single SEND_MULTI LZ message.
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
}
