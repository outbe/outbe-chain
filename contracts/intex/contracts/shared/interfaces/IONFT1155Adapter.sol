// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {MessagingFee, MessagingReceipt} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";

/// @notice Parameters for sending tokens cross-chain.
struct SendParam {
    uint32 dstEid;
    bytes32 to;
    uint256 tokenId;
    uint256 amount;
    bytes extraOptions;
    bytes composeMsg;
}

/// @title IONFT1155Adapter
/// @author Outbe
/// @notice Interface for single-token cross-chain ERC1155 transfers via LayerZero.
/// @dev Token must implement IERC1155Bridgeable and grant RELAYER_ROLE to this adapter.
interface IONFT1155Adapter {
    // --- Events ---
    /// @notice Emitted when tokens are sent cross-chain.
    /// @param guid LayerZero message GUID
    /// @param dstEid Destination endpoint ID
    /// @param from Sender address
    /// @param tokenId Token ID sent
    /// @param amount Number of tokens sent
    event ONFTSent(bytes32 indexed guid, uint32 dstEid, address indexed from, uint256 tokenId, uint256 amount);

    /// @notice Emitted when tokens are received from another chain.
    /// @param guid LayerZero message GUID
    /// @param srcEid Source endpoint ID
    /// @param to Recipient address
    /// @param tokenId Token ID received
    /// @param amount Number of tokens received
    event ONFTReceived(bytes32 indexed guid, uint32 srcEid, address indexed to, uint256 tokenId, uint256 amount);

    /// @notice Emitted when residual native tokens are swept to an owner-chosen recipient.
    /// @param to Recipient of the swept native tokens
    /// @param amount Amount in wei swept to the recipient
    event NativeSwept(address indexed to, uint256 amount);

    // --- Errors ---
    /// @notice Receiver address is zero.
    error InvalidReceiver();
    /// @notice Native-token sweep transfer failed.
    error NativeSweepFailed();
    /// @notice Native-token balance is insufficient for the requested sweep.
    /// @param available Current contract balance
    /// @param requested Amount the owner attempted to sweep
    error NativeBalanceInsufficient(uint256 available, uint256 requested);
    /// @notice Zero address provided.
    /// @param field Field name
    error ZeroAddress(string field);

    // --- Functions ---
    /// @notice Quotes the messaging fee for a cross-chain transfer.
    /// @param _sendParam Transfer parameters
    /// @param _payInLzToken Whether to pay in LZ token
    /// @return Messaging fee quote
    function quoteSend(SendParam calldata _sendParam, bool _payInLzToken) external view returns (MessagingFee memory);

    /// @notice Sends tokens to another chain.
    /// @dev Caller must have approved this adapter for the token.
    /// @param _sendParam Transfer parameters
    /// @param _fee Messaging fee
    /// @param _refundAddress Address for fee refund
    /// @return msgReceipt Messaging receipt
    function send(
        SendParam calldata _sendParam,
        MessagingFee calldata _fee,
        address _refundAddress
    ) external payable returns (MessagingReceipt memory msgReceipt);

    /// @notice Sweep any residual native tokens back to an owner-chosen recipient.
    /// @dev Default OApp `_payNative` reverts on mismatch so no ETH should ever accumulate;
    ///      this exists purely as a defensive recovery hatch.
    /// @param to Recipient address (must be non-zero)
    /// @param amount Amount in wei to sweep; must be ≤ contract balance
    function sweepNative(address payable to, uint256 amount) external;
}
