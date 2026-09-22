// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {PolicyBase} from "kernel-7579-plugins/base/PolicyBase.sol";
import {PackedUserOperation} from "account-abstraction/interfaces/PackedUserOperation.sol";
import {IERC7579Account} from "@zerodev/kernel/interfaces/IERC7579Account.sol";
import {_packValidationData} from "account-abstraction/core/Helpers.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IAccountExecute} from "account-abstraction/interfaces/IAccountExecute.sol";

/// @title WithdrawalLimitPolicy
/// @notice ERC-7579 Policy (module type 5) that enforces per-interval cumulative
///         ERC20 transfer limits on smart accounts.
/// @dev Tracks the total amount of a specific ERC20 token transferred within rolling
///      time windows. Only CALLTYPE_SINGLE calls to `IERC20.transfer` on the configured
///      token are permitted; every other execution is rejected.
/// @author Outbe Team
/// @custom:version 1.0.0
contract WithdrawalLimitPolicy is PolicyBase {
    // -------------------------------------------------------------------------
    // Types
    // -------------------------------------------------------------------------

    /// @notice Configuration stored per (id, wallet) pair on install.
    struct WithdrawalLimitConfig {
        uint256 amountLimit; // token-minor units (e.g. 6-dec); max cumulative transfer per interval
        uint48 interval; // Time window length in seconds
        address token; // ERC20 token address this policy governs
    }

    /// @notice Rolling-window state tracked per (id, wallet) pair.
    struct WithdrawalLimitState {
        uint256 usedAmount; // token-minor units (e.g. 6-dec); cumulative spent in current window
        uint48 windowEnd; // Timestamp when current window expires
    }

    enum Status {
        NA,
        Live,
        Deprecated
    }

    // -------------------------------------------------------------------------
    // Errors
    // -------------------------------------------------------------------------

    error WithdrawalLimitExceeded(uint256 used, uint256 limit);
    error WithdrawalLimitAlreadyInitialized();
    error NonCanonicalOffset(uint256 offset);
    error InvalidWithdrawal();

    // -------------------------------------------------------------------------
    // Events
    // -------------------------------------------------------------------------

    /// @notice Emitted when a metered transfer consumes daily-limit headroom, so off-chain monitoring
    ///         can observe the limit nearing exhaustion on a (possibly compromised) CCA.
    event LimitConsumed(bytes32 indexed id, address indexed wallet, uint256 amount, uint256 used, uint48 windowEnd);
    /// @notice Emitted when an expired window is reset before metering a new transfer.
    event WindowReset(bytes32 indexed id, address indexed wallet, uint48 windowEnd);

    // -------------------------------------------------------------------------
    // State
    // -------------------------------------------------------------------------

    /// @notice Number of active policy IDs installed per wallet.
    mapping(address => uint256) public usedIds;

    /// @notice Lifecycle status per (id, wallet).
    mapping(bytes32 id => mapping(address wallet => Status)) public status;

    /// @notice Config stored per (id, wallet).
    mapping(bytes32 id => mapping(address wallet => WithdrawalLimitConfig)) public configs;

    /// @notice Rolling-window state per (id, wallet).
    mapping(bytes32 id => mapping(address wallet => WithdrawalLimitState)) public states;

    // -------------------------------------------------------------------------
    // PolicyBase hooks
    // -------------------------------------------------------------------------

    /// @inheritdoc PolicyBase
    /// @dev `data` = abi.encode(uint256 amountLimit, uint48 interval, address token)
    function _policyOninstall(bytes32 id, bytes calldata data) internal override {
        if (status[id][msg.sender] == Status.Live) revert WithdrawalLimitAlreadyInitialized();
        (uint256 amountLimit, uint48 interval, address token) = abi.decode(data, (uint256, uint48, address));
        configs[id][msg.sender] = WithdrawalLimitConfig({amountLimit: amountLimit, interval: interval, token: token});
        uint48 blockTs = uint48(block.timestamp);
        uint48 windowEnd = interval > type(uint48).max - blockTs ? type(uint48).max : blockTs + interval;
        states[id][msg.sender] = WithdrawalLimitState({usedAmount: 0, windowEnd: windowEnd});
        status[id][msg.sender] = Status.Live;
        usedIds[msg.sender]++;
    }

    /// @inheritdoc PolicyBase
    function _policyOnUninstall(bytes32 id, bytes calldata) internal override {
        status[id][msg.sender] = Status.Deprecated;
        usedIds[msg.sender]--;
    }

    /// @notice Returns true once at least one policy id is installed for `wallet`.
    /// @dev Not part of the kernel-7579-plugins `IModule`, so this is a plain declaration (no
    ///      `override`); Kernel still invokes it by selector at install/uninstall time.
    function isInitialized(address wallet) external view returns (bool) {
        return usedIds[wallet] > 0;
    }

    // -------------------------------------------------------------------------
    // IPolicy
    // -------------------------------------------------------------------------

    /// @inheritdoc PolicyBase
    /// @dev Enforces the cumulative transfer limit for CALLTYPE_SINGLE ERC20 transfers.
    ///      Rejects non-canonical calls and all operations except the configured token transfer.
    ///      Reverts with WithdrawalLimitExceeded when the limit is breached.
    ///
    ///      userOp.callData layout (Kernel execute):
    ///        [0:4]   = function selector
    ///        [4:36]  = ExecMode (bytes32)
    ///        [36:68] = ABI offset to executionCalldata bytes (= 64)
    ///        [68:100] = length of executionCalldata
    ///        [100:]  = executionCalldata = abi.encodePacked(target(20), value(32), callData)
    function checkUserOpPolicy(bytes32 id, PackedUserOperation calldata userOp)
        external
        payable
        override
        returns (uint256)
    {
        bytes calldata uopCallData = userOp.callData;

        // When a permission has a hook, Kernel requires callData to start with executeUserOp.selector
        // and checks the inner selector at [4:8]. Strip the outer prefix so ExecMode is at [4:36].
        if (uopCallData.length >= 4 && bytes4(uopCallData[0:4]) == IAccountExecute.executeUserOp.selector) {
            uopCallData = uopCallData[4:];
        }

        if (
            status[id][msg.sender] != Status.Live || userOp.sender != msg.sender || uopCallData.length != 228
                || bytes4(uopCallData[:4]) != IERC7579Account.execute.selector
                || bytes32(uopCallData[4:36]) != bytes32(0)
        ) revert InvalidWithdrawal();
        uint256 offset = uint256(bytes32(uopCallData[36:68]));
        if (offset != 64) revert NonCanonicalOffset(offset);
        if (uint256(bytes32(uopCallData[68:100])) != 120) revert InvalidWithdrawal();
        WithdrawalLimitConfig storage cfg = configs[id][msg.sender];
        if (
            address(bytes20(uopCallData[100:120])) != cfg.token || bytes32(uopCallData[120:152]) != bytes32(0)
                || bytes4(uopCallData[152:156]) != IERC20.transfer.selector
                || uint256(bytes32(uopCallData[156:188])) >> 160 != 0 || bytes8(uopCallData[220:228]) != bytes8(0)
        ) revert InvalidWithdrawal();
        uint256 amount = uint256(bytes32(uopCallData[188:220]));

        WithdrawalLimitState storage state = states[id][msg.sender];

        // Reset window if expired
        if (block.timestamp >= state.windowEnd) {
            state.usedAmount = 0;
            uint48 nowTs = uint48(block.timestamp);
            state.windowEnd = cfg.interval > type(uint48).max - nowTs ? type(uint48).max : nowTs + cfg.interval;
            emit WindowReset(id, msg.sender, state.windowEnd);
        }

        uint256 newUsed = state.usedAmount + amount;
        if (newUsed > cfg.amountLimit) revert WithdrawalLimitExceeded(newUsed, cfg.amountLimit);

        // NB: The debit is committed here in the ERC-4337 validation phase, on purpose. The
        // EntryPoint validates every op in a bundle before executing any, so committing at validation
        // is what prevents two bundled ops from each passing a stale-headroom check and together
        // over-spending the daily limit - the exact protection this limit exists to give the owner
        // against a compromised CCA. The accepted trade-off is over-restriction: if the execution
        // later reverts, the debit still stands until the window resets (a misbehaving CCA can waste
        // its own daily headroom). Moving this to a post-execution commit would reopen the
        // intra-bundle over-spend, so it is deliberately kept in validation. See
        // test_W04_RevertingExec_StillDebits.
        state.usedAmount = newUsed;
        emit LimitConsumed(id, msg.sender, amount, newUsed, state.windowEnd);

        // validAfter = 0, validUntil = windowEnd, sigFailed = false (account-abstraction v0.9 packing).
        return _packValidationData(false, state.windowEnd, 0);
    }

    /// @inheritdoc PolicyBase
    function checkSignaturePolicy(bytes32, address, bytes32, bytes calldata) external pure override returns (uint256) {
        return 1;
    }
}
