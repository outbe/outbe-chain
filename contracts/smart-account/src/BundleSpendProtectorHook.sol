// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

import {IHook} from "@zerodev/kernel/interfaces/IERC7579Modules.sol";
import {MODULE_TYPE_HOOK, CALLTYPE_SINGLE, CALLTYPE_BATCH} from "@zerodev/kernel/types/Constants.sol";
import {CallType} from "@zerodev/kernel/types/Types.sol";
import {ExecLib} from "@zerodev/kernel/utils/ExecLib.sol";
import {LibERC7579} from "solady/accounts/LibERC7579.sol";
import {ITokenBundle} from "./interfaces/ITokenBundle.sol";
import {IERC20} from "forge-std/interfaces/IERC20.sol";

/// @notice Execution hook that bounds movement of the bundled (Vault-sourced) portion of the
///         account. A CALLTYPE_SINGLE transfer/transferFrom of a bundled token is gated to its
///         free (non-bundled) balance and approve of a bundled token is rejected; a CALLTYPE_BATCH
///         that moves a bundled token at all is rejected (batches over only free tokens pass).
/// @dev Wired as the root validator execution hook when bundleTokens.length > 0.
///      msgData layout (from Kernel.executeUserOp → preCheck):
///        [0:4]     execute.selector
///        [4:36]    ExecMode (bytes32; byte [4] = callType: 0x00 = SINGLE, 0x01 = BATCH)
///        [36:68]   ABI offset to bytes param (= 64)
///        [68:100]  length of executionCalldata
///        [100:]    executionCalldata (SINGLE: packed address‖uint256‖callData; BATCH: abi.encode(Execution[]))
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

        uint256 execLen = uint256(bytes32(msgData[68:100]));
        if (msgData.length < 100 + execLen) return hex"";
        bytes calldata execCalldata = msgData[100:100 + execLen];

        // Call type is the first byte of ExecMode, at msgData[4].
        CallType callType = CallType.wrap(msgData[4]);

        if (callType == CALLTYPE_SINGLE) {
            // Minimum: target(20) + value(32) + selector(4) = 56 bytes
            if (execCalldata.length < 56) return hex"";
            (address target,, bytes calldata innerCallData) = ExecLib.decodeSingle(execCalldata);
            _enforceFreeBalance(msg.sender, target, innerCallData);
            return hex"";
        }

        if (callType == CALLTYPE_BATCH) {
            // Conservative: reject any batch that moves a bundled token at all. preCheck reads
            // balanceOf once (before the batch runs), so per-sub-call freeBalance gating would
            // be bypassable by splitting one move across sub-calls. Decoded with the same
            // LibERC7579.decodeBatch the kernel executes with, so the hook and executor agree.
            bytes32[] calldata pointers = LibERC7579.decodeBatch(execCalldata);
            for (uint256 i; i < pointers.length; ++i) {
                (address target,, bytes calldata innerCallData) = LibERC7579.getExecution(pointers, i);
                _rejectBundledMove(msg.sender, target, innerCallData);
            }
            return hex"";
        }

        return hex""; // other call types (e.g. delegatecall) unmodified by this hook
    }

    /// @dev CALLTYPE_SINGLE path: gate transfer/transferFrom of a bundled token against its
    ///      freeBalance; reject approve of a bundled token (a static check at grant time cannot
    ///      bound the grantee's later, unhooked transferFrom).
    function _enforceFreeBalance(address account, address target, bytes calldata innerCallData) internal view {
        if (innerCallData.length < 4) return;
        uint256 bundleBalance = BUNDLE_PLUGIN.balanceOf(account, target);
        if (bundleBalance == 0) return; // not a bundled token → no restriction

        bytes4 sel = bytes4(innerCallData[0:4]);
        if (sel == IERC20.approve.selector) revert InsufficientFreeBalance();

        uint256 amount;
        if (sel == IERC20.transfer.selector) {
            // transfer(address,uint256): selector(4) + recipient(32) + amount(32)
            if (innerCallData.length < 68) return;
            amount = uint256(bytes32(innerCallData[36:68]));
        } else if (sel == IERC20.transferFrom.selector) {
            // transferFrom(address,address,uint256): selector(4) + from(32) + to(32) + amount(32)
            if (innerCallData.length < 100) return;
            amount = uint256(bytes32(innerCallData[68:100]));
        } else {
            return; // non-value-moving selector
        }

        uint256 totalBalance = IERC20(target).balanceOf(account);
        uint256 freeBalance = totalBalance > bundleBalance ? totalBalance - bundleBalance : 0;
        if (amount > freeBalance) revert InsufficientFreeBalance();
    }

    /// @dev CALLTYPE_BATCH path: revert if this sub-call moves a bundled token at all.
    function _rejectBundledMove(address account, address target, bytes calldata innerCallData) internal view {
        if (innerCallData.length < 4) return;
        if (BUNDLE_PLUGIN.balanceOf(account, target) == 0) return; // not a bundled token → allowed
        bytes4 sel = bytes4(innerCallData[0:4]);
        if (sel == IERC20.transfer.selector || sel == IERC20.transferFrom.selector || sel == IERC20.approve.selector) {
            revert InsufficientFreeBalance();
        }
    }

    function postCheck(bytes calldata) external payable override {}
}
