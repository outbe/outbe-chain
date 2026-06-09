// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AccessControl} from "@openzeppelin/contracts/access/AccessControl.sol";
import {IPromisLimit} from "./interfaces/IPromisLimit.sol";

/**
 * @title MockPromisLimit
 * @notice Mock implementation of the PromisLimit precompile for Outbe Chain.
 * @dev Stores a running total of unallocated Promis limit.
 *      Will be replaced by a stateful precompile wrapping `x/promislimit` keeper.
 */
contract MockPromisLimit is AccessControl, IPromisLimit {
    /// @notice Granted to Desis, the only caller of `addUnallocatedPromisLimit` (unused-supply
    ///         reporting at clearing).
    bytes32 public constant DESIS_ROLE = keccak256("DESIS_ROLE");

    uint256 private _totalUnallocatedPromisLimit;

    constructor(address defaultAdmin) {
        _grantRole(DEFAULT_ADMIN_ROLE, defaultAdmin);
    }

    /// @inheritdoc IPromisLimit
    function addUnallocatedPromisLimit(uint256 amount) external override onlyRole(DESIS_ROLE) {
        _totalUnallocatedPromisLimit += amount;
        emit UnallocatedPromisLimitAdded(amount, _totalUnallocatedPromisLimit);
    }

    /// @inheritdoc IPromisLimit
    function totalUnallocatedPromisLimit() external view override returns (uint256) {
        return _totalUnallocatedPromisLimit;
    }
}
