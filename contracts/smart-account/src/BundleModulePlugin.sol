// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

import {IERC20} from "forge-std/interfaces/IERC20.sol";
import {IModule} from "@zerodev/kernel/interfaces/IERC7579Modules.sol";
import {IERC7579Account} from "@zerodev/kernel/interfaces/IERC7579Account.sol";
import {MODULE_TYPE_FALLBACK, MODULE_TYPE_EXECUTOR} from "@zerodev/kernel/types/Constants.sol";
import {LibERC7579} from "solady/accounts/LibERC7579.sol";
import {ITokenBundle} from "./interfaces/ITokenBundle.sol";

contract BundleModulePlugin is IModule, ITokenBundle {
    // --- State (keyed by Kernel account address = msg.sender during lifecycle calls) ---
    mapping(address => bool) private installed;
    mapping(address account => address[]) private bundleTokens;
    mapping(address account => mapping(address token => uint256)) private bundleBalance;

    error HasBundleBalance(address token);
    error BundleNotInstalled();
    error TokenNotInBundle(address token);
    error UnauthorizedHook();

    event BundleTransfer(address indexed from, address indexed to, address indexed token, uint256 value);

    constructor() {}

    /// @dev Called by Kernel during module installation.
    ///      When installed as an executor with empty data, this is a no-op.
    ///      When installed as a fallback, `data` = `abi.encode(address[] bundleTokens)`.
    function onInstall(bytes calldata data) external payable override {
        if (data.length == 0) return;
        installed[msg.sender] = true;
        address[] memory tokens = abi.decode(data, (address[]));
        bundleTokens[msg.sender] = tokens;
    }

    function onUninstall(bytes calldata) external payable override {
        address[] memory tokens = bundleTokens[msg.sender];
        for (uint256 i = 0; i < tokens.length; i++) {
            if (bundleBalance[msg.sender][tokens[i]] > 0) {
                revert HasBundleBalance(tokens[i]);
            }
        }
        installed[msg.sender] = false;
    }

    function isModuleType(uint256 id) external pure override returns (bool) {
        return id == MODULE_TYPE_FALLBACK || id == MODULE_TYPE_EXECUTOR;
    }

    function isInitialized(address smartAccount) external view override returns (bool) {
        return installed[smartAccount];
    }

    /// @notice Pull `amount` of `token` from `sender` into the smart account.
    /// @dev Called via Kernel's fallback dispatch (CALLTYPE_SINGLE), so `msg.sender` is the
    ///      smart account. The transfer is executed by the smart account itself via
    ///      `executeFromExecutor`, which requires this plugin to also be installed as an executor.
    ///      Callers must approve the **smart account** (not this plugin) for the transfer amount.
    function topUp(address sender, address token, uint256 amount) external override {
        address thisAccount = msg.sender;
        require(installed[thisAccount], BundleNotInstalled());
        require(isBundleToken(thisAccount, token), TokenNotInBundle(token));
        // NB: enforce check to verify that the user made a topUp with owns funds to enable credis
        require(IERC20(token).balanceOf(thisAccount) >= amount, "Insufficient funds for Credis");

        // update bundle
        // NB: double the amount of the bundle meaning that 50% will be used for purchases and 50% for Coen buys
        bundleBalance[thisAccount][token] += amount * 2;
        emit BundleTransfer(sender, thisAccount, token, amount);

        // Have the smart account (msg.sender) execute transferFrom in its own context so that
        // the ERC20 sees the smart account as the spender, not this singleton plugin.
        bytes32 execMode =
            LibERC7579.encodeMode(LibERC7579.CALLTYPE_SINGLE, LibERC7579.EXECTYPE_DEFAULT, bytes4(0), bytes22(0));
        bytes memory transferCall = abi.encodeCall(IERC20.transferFrom, (sender, thisAccount, amount));
        // ERC-7579 single execution encoding: target(20) ‖ value(32) ‖ callData.
        IERC7579Account(thisAccount).executeFromExecutor(execMode, abi.encodePacked(token, uint256(0), transferCall));
    }

    function balanceOf(address owner, address token) external view override returns (uint256) {
        // NB: this returns the whole balance i.e. for user payments and for coen buys
        return bundleBalance[owner][token];
    }

    /// @notice The registered bundle tokens for `account`.
    /// @dev Used by BundleSpendProtectorHook to enforce the reserve invariant across every bundled
    ///      token in one pass, without needing to parse them out of the execution calldata.
    function bundleTokensOf(address account) external view override returns (address[] memory) {
        return bundleTokens[account];
    }

    /// @notice Returns true if `token` is a registered bundle token for `account`.
    function isBundleToken(address account, address token) public view returns (bool) {
        address[] memory tokens = bundleTokens[account];
        for (uint256 i = 0; i < tokens.length; i++) {
            if (tokens[i] == token) return true;
        }
        return false;
    }

    /// @notice Decrease bundle balance for the calling smart account by min(amount, currentBalance).
    /// @dev Called via Kernel's executeFromExecutor dispatch from dispatchDecreaseBalance,
    ///      so msg.sender is always the smart account.
    function decreaseBundleBalance(address token, uint256 amount) external {
        address thisAccount = msg.sender;
        require(installed[thisAccount], BundleNotInstalled());
        uint256 bal = bundleBalance[thisAccount][token];
        // NB: decrease twice more balance from bundle
        uint256 bandleAmount = amount * 2;
        bundleBalance[thisAccount][token] = bal > bandleAmount ? bal - bandleAmount : 0;
        // TODO: implement spending bundle for buying COENs
    }

    /// @notice Dispatch a decreaseBundleBalance call through the smart account's executor path.
    /// @dev Called by BundleWithdrawHook.postCheck (msg.sender = smartAccount there).
    ///      Uses this plugin's executor registration to call executeFromExecutor, ensuring
    ///      that decreaseBundleBalance is invoked with msg.sender = smartAccount.
    function dispatchDecreaseBalance(address smartAccount, address token, uint256 amount) external {
        bytes32 execMode =
            LibERC7579.encodeMode(LibERC7579.CALLTYPE_SINGLE, LibERC7579.EXECTYPE_DEFAULT, bytes4(0), bytes22(0));
        bytes memory decreaseCall = abi.encodeCall(BundleModulePlugin.decreaseBundleBalance, (token, amount));
        IERC7579Account(smartAccount)
            .executeFromExecutor(execMode, abi.encodePacked(address(this), uint256(0), decreaseCall));
    }
}
