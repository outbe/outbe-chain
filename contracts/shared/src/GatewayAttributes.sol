// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

/// @dev ERC-7786 attribute of the crosschain hub: the destination execution gas of a message. A
/// sender puts `abi.encodeWithSelector(GAS_LIMIT_ATTR, gasLimit)` in `attributes`.
bytes4 constant GAS_LIMIT_ATTR = bytes4(keccak256("executionGasLimit(uint256)"));

/// @dev ERC-7786 attribute of the crosschain hub: the address that gets the extra native fee of a
/// send back. A sender puts `abi.encodeWithSelector(REFUND_ADDRESS_ATTR, user)` in `attributes`.
bytes4 constant REFUND_ADDRESS_ATTR = bytes4(keccak256("refundAddress(address)"));
