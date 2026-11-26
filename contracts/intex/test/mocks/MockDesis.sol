// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IERC7786Recipient} from "@openzeppelin/contracts/interfaces/draft-IERC7786.sol";

/// @notice Stand-in for the Desis precompile as the hub recipient of the bids of a target chain.
/// @dev Tests `vm.etch` its code at `DESIS`, so it keeps no constructor state. It accepts and
///      counts every delivery, and keeps only a hash of the payload to stay inside the bids budgets.
contract MockDesis is IERC7786Recipient {
    uint256 public received;
    bytes public lastSender;
    bytes32 public lastPayloadHash;

    function receiveMessage(bytes32, bytes calldata sender, bytes calldata payload) external payable returns (bytes4) {
        received++;
        lastSender = sender;
        lastPayloadHash = keccak256(payload);
        return IERC7786Recipient.receiveMessage.selector;
    }
}
