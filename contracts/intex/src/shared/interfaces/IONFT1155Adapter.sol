// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @notice Parameters for sending a single ERC-1155 token cross-chain.
struct SendParam {
    uint32 dstChainId;
    bytes32 to;
    uint256 tokenId;
    uint256 amount;
}

/// @title IONFT1155Adapter
/// @author Outbe
/// @notice Interface for single-token cross-chain ERC-1155 transfers over the protocol-agnostic ERC-7786 bridge.
/// @dev Token must implement IERC1155Bridgeable and grant RELAYER_ROLE to this adapter. Sends burn on the source and
///      mint on the paired adapter registered as the remote messenger for a chainId.
interface IONFT1155Adapter {
    // --- Events ---
    /// @notice Emitted when a token is sent cross-chain.
    /// @param sendId Bridge send identifier.
    /// @param dstChainId Destination chainId.
    /// @param from Sender address.
    /// @param tokenId Token ID sent.
    /// @param amount Number of tokens sent.
    event ONFTSent(bytes32 indexed sendId, uint32 dstChainId, address indexed from, uint256 tokenId, uint256 amount);

    /// @notice Emitted when a token is received from another chain.
    /// @param receiveId Bridge message identifier.
    /// @param srcChainId Source chainId.
    /// @param to Recipient address.
    /// @param tokenId Token ID received.
    /// @param amount Number of tokens received.
    event ONFTReceived(
        bytes32 indexed receiveId, uint32 srcChainId, address indexed to, uint256 tokenId, uint256 amount
    );

    /// @notice Emitted when residual native tokens are swept to an admin-chosen recipient.
    /// @param to Recipient of the swept native tokens.
    /// @param amount Amount in wei swept to the recipient.
    event NativeSwept(address indexed to, uint256 amount);

    // --- Errors ---
    /// @notice Receiver address is zero.
    error InvalidReceiver();
    /// @notice Native-token sweep transfer failed.
    error NativeSweepFailed();
    /// @notice Native-token balance is insufficient for the requested sweep.
    /// @param available Current contract balance.
    /// @param requested Amount the admin attempted to sweep.
    error NativeBalanceInsufficient(uint256 available, uint256 requested);
    /// @notice Zero address provided.
    /// @param field Field name.
    error ZeroAddress(string field);

    // --- Functions ---
    /// @notice Register (or clear) the matching adapter on `chainId` as an ERC-7930 interoperable address.
    /// @param chainId Destination/source chainId.
    /// @param interop ERC-7930 interoperable address (empty to clear).
    function setRemoteMessenger(uint32 chainId, bytes calldata interop) external;

    /// @notice Quotes the native fee for a cross-chain transfer.
    /// @param _sendParam Transfer parameters.
    /// @return fee Native fee the bridge requires.
    function quoteSend(SendParam calldata _sendParam) external view returns (uint256 fee);

    /// @notice Sends a token to another chain. Caller must have approved this adapter and funds the fee via
    ///         `msg.value`.
    /// @param _sendParam Transfer parameters.
    /// @return sendId Bridge send identifier.
    function send(SendParam calldata _sendParam) external payable returns (bytes32 sendId);

    /// @notice Sweep any residual native tokens back to an admin-chosen recipient.
    /// @param to Recipient address (must be non-zero).
    /// @param amount Amount in wei to sweep; must be ≤ contract balance.
    function sweepNative(address payable to, uint256 amount) external;
}
