// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {IPolicy, IHook} from "@zerodev/kernel/interfaces/IERC7579Modules.sol";
import {IERC7579Account} from "@zerodev/kernel/interfaces/IERC7579Account.sol";
import {PackedUserOperation} from "account-abstraction/interfaces/PackedUserOperation.sol";
import {IAccountExecute} from "account-abstraction/interfaces/IAccountExecute.sol";

/// @notice Owner executions require a confirmed request at least five minutes old.
/// @dev Kernel ROOT mode bypasses hooks, so only the hooked permission path is accepted.
contract ExecutionDelayPolicy is IPolicy, IHook {
    uint48 public constant EXECUTION_DELAY = 5 minutes;
    mapping(address => uint256) public generation;
    mapping(address => bytes32) public permission;
    mapping(address => bool) public hooked;
    mapping(address => mapping(bytes32 => uint48)) public readyAt;

    error InvalidExecution();
    error NotInstalled();
    error AlreadyScheduled();
    error NotReady();
    event Scheduled(address indexed account, bytes32 indexed operationId, uint48 readyAt, bytes execution);
    event Cancelled(address indexed account, bytes32 indexed operationId);
    event Executed(address indexed account, bytes32 indexed operationId);

    function onInstall(bytes calldata data) external payable {
        if (data.length == 0) {
            if (hooked[msg.sender]) revert InvalidExecution();
            hooked[msg.sender] = true;
        } else {
            if (data.length != 32 || permission[msg.sender] != bytes32(0)) revert InvalidExecution();
            bytes32 id = bytes32(data);
            if (id == bytes32(0)) revert InvalidExecution();
            permission[msg.sender] = id;
            generation[msg.sender]++;
        }
    }

    function onUninstall(bytes calldata data) external payable {
        if (data.length == 0) {
            hooked[msg.sender] = false;
        } else {
            if (data.length != 32 || bytes32(data) != permission[msg.sender]) revert InvalidExecution();
            delete permission[msg.sender];
        }
        generation[msg.sender]++;
    }

    function isModuleType(uint256 id) external pure returns (bool) {
        return id == 4 || id == 5;
    }

    function isInitialized(address account) external view returns (bool) {
        return permission[account] != bytes32(0) || hooked[account];
    }

    function operationId(address account, bytes calldata execution) public view returns (bytes32) {
        return keccak256(abi.encode(block.chainid, account, generation[account], _normalize(execution)));
    }

    function schedule(bytes calldata execution) external returns (bytes32 id) {
        _installed(msg.sender);
        if (execution.length < 4) revert InvalidExecution();
        id = operationId(msg.sender, execution);
        if (readyAt[msg.sender][id] != 0) revert AlreadyScheduled();
        // Explicit bound before narrowing the timestamp to the ERC-4337 time type.
        if (block.timestamp > type(uint48).max - EXECUTION_DELAY) revert InvalidExecution();
        uint48 time = uint48(block.timestamp + EXECUTION_DELAY);
        readyAt[msg.sender][id] = time;
        emit Scheduled(msg.sender, id, time, _normalize(execution));
    }

    function cancel(bytes32 id) external {
        _installed(msg.sender);
        if (readyAt[msg.sender][id] == 0) revert NotReady();
        delete readyAt[msg.sender][id];
        emit Cancelled(msg.sender, id);
    }

    function checkUserOpPolicy(bytes32 id, PackedUserOperation calldata op) external payable returns (uint256) {
        _installed(msg.sender);
        // Nonce: mode(1), type(1), permission(4), zero padding(16), key(2), sequence(8).
        // Reject ROOT and replayable/enable modes; ROOT never installs the execution hook.
        if (
            op.sender != msg.sender || id != permission[msg.sender] || op.nonce >> 240 != 2
                || bytes4(bytes32(op.nonce << 16)) != bytes4(id) || op.callData.length < 8
                || bytes4(op.callData[:4]) != IAccountExecute.executeUserOp.selector
        ) {
            revert InvalidExecution();
        }
        bytes calldata execution = op.callData[4:];
        if (!_management(execution)) _ready(msg.sender, operationId(msg.sender, execution));
        return 0;
    }

    function checkSignaturePolicy(bytes32, address, bytes32, bytes calldata) external pure returns (uint256) {
        return 1;
    }

    function preCheck(address, uint256 value, bytes calldata execution) external payable returns (bytes memory) {
        _installed(msg.sender);
        if (value == 0 && _management(execution)) return "";
        bytes32 id = operationId(msg.sender, execution);
        _ready(msg.sender, id);
        delete readyAt[msg.sender][id];
        // Hook and inner execution share a revert frame: a failed execution restores the request.
        emit Executed(msg.sender, id);
        return "";
    }

    function postCheck(bytes calldata) external payable {}

    function _installed(address account) private view {
        if (permission[account] == bytes32(0) || !hooked[account]) revert NotInstalled();
    }

    function _ready(address account, bytes32 id) private view {
        uint48 time = readyAt[account][id];
        if (time == 0 || block.timestamp < time) revert NotReady();
    }

    function _normalize(bytes calldata execution) private pure returns (bytes calldata) {
        if (execution.length >= 4 && bytes4(execution[:4]) == IAccountExecute.executeUserOp.selector) {
            return execution[4:];
        }
        return execution;
    }

    /// @dev Only a canonical, standalone, zero-value CALL to this module can bypass the delay.
    function _management(bytes calldata execution) private view returns (bool) {
        if (
            execution.length < 156 || bytes4(execution[:4]) != IERC7579Account.execute.selector
                || bytes32(execution[4:36]) != bytes32(0) || uint256(bytes32(execution[36:68])) != 64
        ) return false;
        uint256 length = uint256(bytes32(execution[68:100]));
        if (length < 56 || length > execution.length - 100 || execution.length != 100 + (length + 31) / 32 * 32) {
            return false;
        }
        if (address(bytes20(execution[100:120])) != address(this) || bytes32(execution[120:152]) != bytes32(0)) {
            return false;
        }
        bytes calldata callData = execution[152:100 + length];
        bytes4 selector = bytes4(callData[:4]);
        if (selector == this.cancel.selector) return callData.length == 36;
        if (selector != this.schedule.selector || callData.length < 68 || uint256(bytes32(callData[4:36])) != 32) {
            return false;
        }
        uint256 innerLength = uint256(bytes32(callData[36:68]));
        return innerLength <= callData.length - 68 && callData.length == 68 + (innerLength + 31) / 32 * 32;
    }
}
