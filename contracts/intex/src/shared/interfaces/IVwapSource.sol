// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title IVwapSource
/// @author Outbe
/// @notice Finalized daily COEN VWAPs the Intex metadata derives qualification from.
interface IVwapSource {
    /// @notice Highest daily VWAP from `fromUtcDay` (yyyymmdd) on; 0 when none is known.
    function maxUtcDayVwapSince(uint16 isoCode, uint32 fromUtcDay) external view returns (uint256);
}
