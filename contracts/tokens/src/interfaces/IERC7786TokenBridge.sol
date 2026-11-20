// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @title IERC7786TokenBridge
/// @notice Client surface of the ERC-7786 fungible token bridge.
interface IERC7786TokenBridge {
    /// @notice The one token the bridge moves.
    function token() external view returns (IERC20);

    /// @notice The native fee a `sendAndCall` with the same arguments requires.
    function quoteSend(uint32 destinationDomain, address to, uint256 amount, bytes calldata extraData, uint256 gasLimit)
        external
        view
        returns (uint256);

    /// @notice Deliver `amount` to `to` on `destinationDomain`.
    function send(uint32 destinationDomain, address to, uint256 amount) external payable returns (bytes32 sendId);

    /// @notice Deliver `amount` to `to` on `destinationDomain` and invoke its receiver hook with `extraData`.
    function sendAndCall(
        uint32 destinationDomain,
        address to,
        uint256 amount,
        bytes calldata extraData,
        uint256 gasLimit
    ) external payable returns (bytes32 sendId);

    /// @notice Sends `amount`, which the token has just burned from 0x…dEaD, to 0x…dEaD on
    ///         `destinationDomain`. Only the token can call it.
    /// @param refundTo Gets the part of `msg.value` above the fee back.
    function sendBurned(uint32 destinationDomain, uint256 amount, address refundTo)
        external
        payable
        returns (bytes32 sendId);
}
