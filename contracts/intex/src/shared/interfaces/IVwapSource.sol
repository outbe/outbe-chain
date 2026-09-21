// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title IVwapSource
/// @author Outbe
/// @notice Finalized daily COEN VWAPs, read by the Intex metadata to derive a series' qualification.
interface IVwapSource {
    /// @notice The highest finalized daily VWAP of COEN in `isoCode` (six decimals) over the UTC days from
    ///         `fromUtcDay` (yyyymmdd) on; 0 when no such day is known.
    function maxUtcDayVwapSince(uint16 isoCode, uint32 fromUtcDay) external view returns (uint256);
}
