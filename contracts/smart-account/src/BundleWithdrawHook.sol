// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;
import {IHook} from "@zerodev/kernel/interfaces/IERC7579Modules.sol";
import {MODULE_TYPE_HOOK} from "@zerodev/kernel/types/Constants.sol";
import {ITokenBundle} from "./interfaces/ITokenBundle.sol";
import {IERC7579Account} from "@zerodev/kernel/interfaces/IERC7579Account.sol";

/// @notice CCA permissions may only submit independently authorized custody payments.
contract BundleWithdrawHook is IHook {
    address public immutable BUNDLE_PLUGIN;
    mapping(address => bool) public installed;
    error InvalidPaymentCall();

    constructor(address plugin) {
        BUNDLE_PLUGIN = plugin;
    }

    function onInstall(bytes calldata) external payable override {
        installed[msg.sender] = true;
    }

    function onUninstall(bytes calldata) external payable override {
        installed[msg.sender] = false;
    }

    function isModuleType(uint256 id) external pure override returns (bool) {
        return id == MODULE_TYPE_HOOK;
    }

    function isInitialized(address account) external view override returns (bool) {
        return installed[account];
    }

    function preCheck(address, uint256, bytes calldata data) external payable override returns (bytes memory) {
        require(data.length >= 100 && bytes4(data[:4]) == IERC7579Account.execute.selector, InvalidPaymentCall());
        // Only canonical SINGLE/DEFAULT calls, with no mode payload or ETH transfer.
        require(bytes32(data[4:36]) == bytes32(0) && uint256(bytes32(data[36:68])) == 64, InvalidPaymentCall());
        (, bytes memory execution) = abi.decode(data[4:], (bytes32, bytes));
        require(execution.length >= 56, InvalidPaymentCall());
        address target;
        uint256 value;
        bytes4 selector;
        assembly {
            target := shr(96, mload(add(execution, 32)))
            value := mload(add(execution, 52))
            selector := mload(add(execution, 84))
        }
        require(target == BUNDLE_PLUGIN && value == 0 && selector == ITokenBundle.spend.selector, InvalidPaymentCall());
        return "";
    }
    function postCheck(bytes calldata) external payable override {}
}
