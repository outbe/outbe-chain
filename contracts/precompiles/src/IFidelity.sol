// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IFidelity {

    /// Fidelity Index for `account`.
    /// Computed on-demand on Retention Component at
    /// the current block timestamp
    function getFidelityIndex(address account) external view returns (uint256);

    /// Fidelity Index for `account`.
    /// Computed on-demand on Retention Component at
    /// the given timestamp
    function getFidelityIndexAt(address account, uint64 timestamp) external view returns (uint256);

    /// Returns fidelity index decimals precision
    function decimals() external view returns (uint8);

    /// Synthetic maximum Fidelity Index (saturating RCFI) at `timestamp`
    function maxFidelityIndexAt(uint64 timestamp) external view returns (uint256);

    /// Lowest league (inclusive)
    function minLeague() external view returns (uint16);

    /// Highest league (inclusive)
    function maxLeague() external view returns (uint16);

    /// League for `account` at the current block timestamp
    function league(address account) external view returns (uint16);
}
