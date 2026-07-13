// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

/// @notice Minimal interface for the composed-transfer token bridge used to route auction proceeds.
/// @dev Vendored from the token-bridge project (separate foundry project, no cross-project remapping).
interface IERC7786TokenBridge {
    /// @notice Deliver `amount` to `to` on `destinationDomain` and invoke its receiver hook with `extraData`.
    function sendAndCall(
        uint32 destinationDomain,
        address to,
        uint256 amount,
        bytes calldata extraData,
        uint256 gasLimit
    ) external payable returns (bytes32 sendId);

    /// @notice Quote the native fee `sendAndCall` requires for the same arguments.
    function quoteSendAndCall(
        uint32 destinationDomain,
        address to,
        uint256 amount,
        bytes calldata extraData,
        uint256 gasLimit
    ) external view returns (uint256);
}
