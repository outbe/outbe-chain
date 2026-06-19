// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IFidelity {
    /// Retention Component of Fidelity Index for `account`, in decayed days
    /// (0..L, L ≈ 526). Computed on-demand from the account's cohort ledger at
    /// the current block timestamp.
    function getRcfi(address account) external view returns (uint64);
}
