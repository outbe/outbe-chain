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

    /// Synthetic maximum (saturating) RCFI at `timestamp`: the decayed age of
    /// the earliest-qualified account on the chain. An upper bound on every
    /// account's RCFI at that time; defines the top of the league range
    /// `[0, maxRcfiAt(timestamp)]`. Same `decimals()` scale as `getRcfi`.
    function maxRcfiAt(uint64 timestamp) external view returns (uint256);

    /// Lowest league id (inclusive).
    function minLeague() external view returns (uint16);

    /// Highest league id (inclusive). Leagues span `[minLeague, maxLeague]`.
    function maxLeague() external view returns (uint16);

    /// League tier for `account` at the current block timestamp: the slot its
    /// RCFI lands in when `[0, maxRcfiAt(now)]` is split into equal tiers.
    /// In `[minLeague, maxLeague]`.
    function league(address account) external view returns (uint16);
}
