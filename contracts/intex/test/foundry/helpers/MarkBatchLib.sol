// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @dev Builds the series batches MARK_CALLED and MARK_QUALIFIED carry.
library MarkBatchLib {
    /// @notice A batch of one series.
    function one(bytes14 seriesId) internal pure returns (bytes14[] memory batch) {
        batch = new bytes14[](1);
        batch[0] = seriesId;
    }

    /// @notice A batch of two series, as a day with two issuance currencies produces.
    function two(bytes14 first, bytes14 second) internal pure returns (bytes14[] memory batch) {
        batch = new bytes14[](2);
        batch[0] = first;
        batch[1] = second;
    }

    /// @notice A batch of `count` series, each id derived from `prefix` by its index.
    function sized(bytes14 prefix, uint256 count) internal pure returns (bytes14[] memory batch) {
        batch = new bytes14[](count);
        for (uint256 i = 0; i < count; ++i) {
            batch[i] = bytes14(bytes32(prefix) | bytes32(uint256(i + 1)));
        }
    }
}
