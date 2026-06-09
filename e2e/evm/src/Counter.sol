// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/// @notice Test counter contract for the outbe sub-call smoke test.
///
/// The counter starts at 0. `inc(x)` adds `x` to the counter,
/// reverting with `NegativeNotAllowed(x)` if `x < 0`. `view_value()`
/// returns the current counter as `int256`.
contract Counter {
    int256 public counter;

    error NegativeNotAllowed(int256 value);

    function inc(int256 x) external {
        if (x < 0) revert NegativeNotAllowed(x);
        counter += x;
    }

    function view_value() external view returns (int256) {
        return counter;
    }
}
