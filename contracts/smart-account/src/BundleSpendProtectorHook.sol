// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

import {IHook} from "@zerodev/kernel/interfaces/IERC7579Modules.sol";
import {MODULE_TYPE_HOOK, CALLTYPE_SINGLE} from "@zerodev/kernel/types/Constants.sol";
import {CallType} from "@zerodev/kernel/types/Types.sol";
import {ExecLib} from "@zerodev/kernel/utils/ExecLib.sol";
import {ITokenBundle} from "./interfaces/ITokenBundle.sol";
import {IERC20} from "forge-std/interfaces/IERC20.sol";

/// @notice Execution hook that blocks ERC20 transfers exceeding free (non-bundled) balance.
/// @dev Wired as the root validator execution hook when bundleTokens.length > 0.
///      msgData layout (from Kernel.executeUserOp → preCheck):
///        [0:4]     execute.selector
///        [4:36]    ExecMode (bytes32; byte [4] = callType: 0x00 = CALLTYPE_SINGLE)
///        [36:68]   ABI offset to bytes param (= 64)
///        [68:100]  length of executionCalldata
///        [100:]    executionCalldata (packed: address‖uint256‖callData)
contract BundleSpendProtectorHook is IHook {
    error InsufficientFreeBalance();

    ITokenBundle public immutable BUNDLE_PLUGIN;
    mapping(address account => bool) public installed;

    constructor(address bundlePlugin) {
        BUNDLE_PLUGIN = ITokenBundle(bundlePlugin);
    }

    // IModule
    function onInstall(bytes calldata) external payable override {
        installed[msg.sender] = true;
    }

    function onUninstall(bytes calldata) external payable override {
        installed[msg.sender] = false;
    }

    function isModuleType(uint256 id) external pure override returns (bool) {
        return id == MODULE_TYPE_HOOK;
    }

    function isInitialized(address a) external view override returns (bool) {
        return installed[a];
    }

    // IHook
    /// @dev msg.sender = the Kernel account executing the UserOp.
    function preCheck(address, uint256, bytes calldata msgData) external payable override returns (bytes memory) {
        // Need: selector(4) + ExecMode(32) + offset(32) + length(32) = 100 bytes minimum
        if (msgData.length < 100) return hex"";

        // TODO check other call types
        // Check CALLTYPE_SINGLE: first byte of ExecMode is at msgData[4]
        if (CallType.wrap(msgData[4]) != CALLTYPE_SINGLE) return hex"";

        uint256 execLen = uint256(bytes32(msgData[68:100]));
        if (msgData.length < 100 + execLen) return hex"";

        bytes calldata execCalldata = msgData[100:100 + execLen];

        // Minimum: target(20) + value(32) + selector(4) = 56 bytes
        if (execCalldata.length < 56) return hex"";

        (address target,, bytes calldata innerCallData) = ExecLib.decodeSingle(execCalldata);

        // TODO inspect other call types for example allowance
        // Only inspect ERC20 transfer(address,uint256) calls
        if (innerCallData.length < 4) return hex"";
        if (bytes4(innerCallData[0:4]) != IERC20.transfer.selector) return hex"";

        // transfer(address,uint256) ABI: selector(4) + recipient(32) + amount(32)
        if (innerCallData.length < 68) return hex"";
        uint256 amount = uint256(bytes32(innerCallData[36:68]));

        uint256 bundleBalance = BUNDLE_PLUGIN.balanceOf(msg.sender, target);
        if (bundleBalance == 0) return hex""; // no bundle balance → no restriction

        uint256 totalBalance = IERC20(target).balanceOf(msg.sender);
        uint256 freeBalance = totalBalance > bundleBalance ? totalBalance - bundleBalance : 0;

        if (amount > freeBalance) revert InsufficientFreeBalance();
        return hex"";
    }

    function postCheck(bytes calldata) external payable override {}
}
