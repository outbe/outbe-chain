// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title DateKey
/// @author Outbe
/// @notice yyyymmdd UTC day keys.
library DateKey {
    /// @notice The yyyymmdd UTC day of `timestamp`.
    function fromTimestamp(uint256 timestamp) internal pure returns (uint32) {
        return _fromDays(timestamp / 1 days);
    }

    /// @dev Hinnant's civil_from_days.
    function _fromDays(uint256 z) private pure returns (uint32) {
        z += 719_468;
        uint256 era = z / 146_097;
        uint256 doe = z - era * 146_097;
        uint256 yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        uint256 doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        uint256 mp = (5 * doy + 2) / 153;
        uint256 day = doy - (153 * mp + 2) / 5 + 1;
        uint256 month = mp < 10 ? mp + 3 : mp - 9;
        uint256 year = yoe + era * 400 + (month <= 2 ? 1 : 0);
        // forge-lint: disable-next-line(unsafe-typecast) -- a yyyymmdd key fits in uint32 for years below 429,496
        return uint32(year * 10_000 + month * 100 + day);
    }
}
