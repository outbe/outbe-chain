// SPDX-License-Identifier: MIT

pragma solidity ^0.8.30;

import {Vm} from "forge-std/Vm.sol";

/// @dev Reads the MessageSent and MessageExecuted events from recorded logs, to check the message id.
library MessageLogs {
    bytes32 internal constant SENT = keccak256("MessageSent(bytes32,bytes,bytes,bytes,uint256,bytes[])");
    bytes32 internal constant EXECUTED = keccak256("MessageExecuted(bytes32,address)");

    /// @dev The first MessageSent from `emitter`: its sendId, sender and payload.
    function sent(Vm.Log[] memory logs, address emitter)
        internal
        pure
        returns (bytes32 sendId, bytes memory sender, bytes memory payload)
    {
        for (uint256 i = 0; i < logs.length; ++i) {
            if (logs[i].emitter != emitter || logs[i].topics[0] != SENT) continue;
            (sender,, payload,,) = abi.decode(logs[i].data, (bytes, bytes, bytes, uint256, bytes[]));
            return (logs[i].topics[1], sender, payload);
        }
        revert("MessageSent not found");
    }

    /// @dev The receiveId of the first MessageExecuted from `hub`.
    function executed(Vm.Log[] memory logs, address hub) internal pure returns (bytes32) {
        for (uint256 i = 0; i < logs.length; ++i) {
            if (logs[i].emitter == hub && logs[i].topics[0] == EXECUTED) return logs[i].topics[1];
        }
        revert("MessageExecuted not found");
    }

    /// @dev The hub id that an indexer derives from the adapter's MessageSent (sender = source hub,
    /// payload = wrapped payload).
    function hubId(bytes memory adapterSender, bytes memory adapterPayload) internal pure returns (bytes32) {
        return keccak256(abi.encode(adapterSender, adapterPayload));
    }
}
