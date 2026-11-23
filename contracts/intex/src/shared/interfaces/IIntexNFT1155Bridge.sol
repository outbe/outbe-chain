// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @notice Parameters for sending a batch of tokens to a SINGLE recipient.
struct BatchSendParam {
    uint32 dstChainId;
    bytes32 to;
    uint256[] tokenIds;
    uint256[] units;
}

/// @notice Parameters for sending tokens to MULTIPLE recipients in one transaction.
/// @dev Each recipient can receive different token IDs and units.
struct MultiRecipientSendParam {
    uint32 dstChainId;
    bytes32[] recipients;
    uint256[] tokenIds;
    uint256[] units;
}

/// @notice Parameters for sending a SINGLE token type to one recipient (its own `SEND_ONE` message).
struct SendParam {
    uint32 dstChainId;
    bytes32 to;
    uint256 tokenId;
    uint256 units;
}

/// @title IIntexNFT1155Bridge
/// @author Outbe
/// @notice Interface for batch cross-chain ERC1155 transfers over the protocol-agnostic ERC-7786
///         bridge.
/// @dev Supports single-recipient batch and multi-recipient modes. Sends burn on the source and
///      mint on the paired adapter registered as the remote messenger for a chainId. `send*`
///      return the bridge `sendId`. `quote*` return the native fee.
interface IIntexNFT1155Bridge {
    // --- Events ---
    /// @notice Emitted when units of one series leave for one recipient.
    /// @param owner The account whose units this chain burned.
    event IntexCrosschainSent(
        bytes32 indexed sendId,
        uint32 indexed dstChainId,
        address indexed owner,
        bytes32 to,
        uint256 intexId,
        uint32 units
    );

    /// @notice Emitted when units of several series leave for one recipient.
    /// @param owner The account whose units this chain burned.
    event IntexCrosschainBatchSent(
        bytes32 indexed sendId,
        uint32 indexed dstChainId,
        address indexed owner,
        bytes32 to,
        uint256[] intexIds,
        uint32[] units
    );

    /// @notice Emitted when units leave for several recipients.
    /// @param owner The account whose units this chain burned.
    event IntexCrosschainMultiSent(
        bytes32 indexed sendId,
        uint32 indexed dstChainId,
        address indexed owner,
        bytes32[] recipients,
        uint256[] intexIds,
        uint32[] units
    );

    /// @notice Emitted when a message for one recipient is received. A failed item has its own
    ///         {IntexCrosschainReceiveFailed}.
    event IntexCrosschainBatchReceived(
        bytes32 indexed receiveId, uint32 indexed srcChainId, address indexed to, uint256[] intexIds, uint32[] units
    );

    /// @notice Emitted when a message for several recipients is received. A failed item has its own
    ///         {IntexCrosschainReceiveFailed}.
    event IntexCrosschainMultiReceived(
        bytes32 indexed receiveId, uint32 indexed srcChainId, address[] recipients, uint256[] intexIds, uint32[] units
    );

    /// @notice Emitted when the units of one item are minted: a single send, or the retry of an item that failed.
    event IntexCrosschainReceived(
        bytes32 indexed receiveId, uint32 indexed srcChainId, address indexed to, uint256 intexId, uint32 units
    );

    /// @notice Emitted when one item of a received message cannot be minted, and the bridge keeps
    ///         it for {retryCrosschainMint} or {reclaimToSource}.
    /// @param idx Position of the item in the message.
    /// @param errorData Raw revert data of `token.crosschainMint`.
    event IntexCrosschainReceiveFailed(
        bytes32 indexed receiveId,
        uint32 indexed srcChainId,
        uint256 idx,
        address indexed to,
        uint256 intexId,
        uint32 units,
        bytes errorData
    );

    /// @notice Emitted when a kept item goes back to its source chain.
    /// @param receiveId The message the item came in.
    /// @param idx Position of the item in that message.
    /// @param to The address the source chain mints the units to.
    event IntexCrosschainSentBack(
        bytes32 indexed sendId,
        uint32 indexed dstChainId,
        bytes32 indexed receiveId,
        uint256 idx,
        bytes32 to,
        uint256 intexId,
        uint32 units
    );

    // --- Errors ---
    /// @notice Receiver address is zero.
    error InvalidReceiver();
    /// @notice Array lengths do not match.
    error ArrayLengthMismatch();
    /// @notice Empty batch provided.
    error EmptyBatch();
    /// @notice Zero address provided.
    /// @param field Field name.
    error ZeroAddress(string field);
    /// @notice Inbound `msgType` is not in the SEND / SEND_MULTI set.
    /// @dev Wire-format reverts (`UnsupportedBodyVersion`, `MalformedAddress`, `BatchTooLarge`,
    ///      `InvalidPayloadLength`, `ArrayLengthMismatch`) are owned by `IntexNFT1155BridgeCodec`.
    /// @param got The unsupported message-type tag received.
    error UnknownMsgType(uint8 got);
    /// @notice An external caller invoked `crosschainMintOne`. Only `address(this)` is allowed.
    /// @dev `crosschainMintOne` is a self-call shim. The inbound handler uses it to isolate
    ///      per-item `token.crosschainMint` reverts. Exposing it externally would let anyone mint
    ///      tokens for arbitrary recipients.
    error NotSelf();
    /// @notice No failed-crosschainMint entry exists for `(receiveId, idx)`.
    /// @param receiveId Inbound bridge message id being retried.
    /// @param idx Position in the original batch with no parked failed-crosschainMint slot.
    error NoSuchFailedCrosschainMint(bytes32 receiveId, uint256 idx);

    /// @notice Register (or clear) the matching adapter on `chainId` as an ERC-7930 interoperable
    ///         address.
    /// @param chainId Destination/source chainId.
    /// @param interop ERC-7930 interoperable address (empty to clear).
    function registerRemoteMessenger(uint32 chainId, bytes calldata interop) external;

    // --- Single-recipient batch ---
    /// @notice Quotes the native fee for a batch cross-chain transfer.
    /// @param _sendParam Batch send parameters.
    /// @notice Native fee to send a single token type to one recipient.
    /// @param _sendParam Single send parameters.
    /// @return fee Native fee the bridge requires.
    function quoteSend(SendParam calldata _sendParam) external view returns (uint256 fee);

    /// @notice Sends a single token type to one recipient on another chain. Caller funds the fee
    ///         via `msg.value`.
    /// @param _sendParam Single send parameters.
    /// @return sendId Bridge send identifier.
    function send(SendParam calldata _sendParam) external payable returns (bytes32 sendId);

    /// @return fee Native fee the bridge requires.
    function quoteBatchSend(BatchSendParam calldata _sendParam) external view returns (uint256 fee);

    /// @notice Sends multiple token types to one recipient on another chain. Caller funds the fee
    ///         via `msg.value`.
    /// @param _sendParam Batch send parameters.
    /// @return sendId Bridge send identifier.
    function batchSend(BatchSendParam calldata _sendParam) external payable returns (bytes32 sendId);

    // --- Multi-recipient ---
    /// @notice Quotes the native fee for a multi-recipient cross-chain transfer.
    /// @param _sendParam Multi-recipient send parameters.
    /// @return fee Native fee the bridge requires.
    function quoteMultiSend(MultiRecipientSendParam calldata _sendParam) external view returns (uint256 fee);

    /// @notice Sends tokens to multiple recipients on another chain. Caller funds the fee via
    ///         `msg.value`.
    /// @param _sendParam Multi-recipient send parameters.
    /// @return sendId Bridge send identifier.
    function multiSend(MultiRecipientSendParam calldata _sendParam) external payable returns (bytes32 sendId);
}
