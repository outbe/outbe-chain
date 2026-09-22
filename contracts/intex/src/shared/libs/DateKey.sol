// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title DateKey
/// @author Outbe
/// @notice Calendar arithmetic on yyyymmdd UTC day keys, the unit the Oracle finalizes daily VWAPs in.
library DateKey {
    /// @notice The UTC day before `key`.
    function previousDateKey(uint32 key) internal pure returns (uint32) {
        uint32 year = key / 10_000;
        uint32 month = (key / 100) % 100;
        if (key % 100 > 1) return key - 1;
        if (month > 1) return year * 10_000 + (month - 1) * 100 + _daysInMonth(year, month - 1);
        return (year - 1) * 10_000 + 1231;
    }

    /// @notice The first UTC day a right issued at `timestamp` holds in full: its own day at exact midnight,
    ///         the next one otherwise.
    function firstFullDay(uint256 timestamp) internal pure returns (uint32) {
        return _fromDays((timestamp + 1 days - 1) / 1 days);
    }

    /// @dev yyyymmdd of the day `z` days after 1970-01-01 (Hinnant's civil_from_days).
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

    function _daysInMonth(uint32 year, uint32 month) private pure returns (uint32) {
        if (month == 2) return (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 ? 29 : 28;
        if (month == 4 || month == 6 || month == 9 || month == 11) return 30;
        return 31;
    }
}
