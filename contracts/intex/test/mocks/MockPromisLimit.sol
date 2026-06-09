// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IPromisLimit} from "../../contracts/outbe/interfaces/IPromisLimit.sol";

/**
 * @title MockPromisLimit
 * @notice Minimal IPromisLimit stand-in for Desis clearing tests. Accumulates the unallocated
 *         Promis reported after clearing so the unused-supply branch can be exercised and asserted.
 */
contract MockPromisLimit is IPromisLimit {
    uint256 public total;

    function addUnallocatedPromisLimit(uint256 amount) external override {
        total += amount;
    }

    function totalUnallocatedPromisLimit() external view override returns (uint256) {
        return total;
    }
}
