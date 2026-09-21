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

    function _daysInMonth(uint32 year, uint32 month) private pure returns (uint32) {
        if (month == 2) return (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 ? 29 : 28;
        if (month == 4 || month == 6 || month == 9 || month == 11) return 30;
        return 31;
    }
}
