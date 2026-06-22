// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IFidelity {

    /// Retention Component of Fidelity Index for `account`, in decayed days.
    /// Computed on-demand from the account's cohort ledger at
    /// the current block timestamp.
    function getRcfi(address account) external view returns (uint256);

    /// Retention Component of Fidelity Index for `account`, in decayed days.
    /// Computed on-demand from the account's cohort ledger at
    /// the given timestamp.
    function getRcfiAt(address account, uint64 timestamp) external view returns (uint256);

    /// Returns fidelity index decimals precision
    function decimals() external view returns (uint8);
}
