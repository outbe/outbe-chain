// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IVwapSource} from "@contracts/shared/interfaces/IVwapSource.sol";

/// @dev Answers a price only for the exact day it is asked from, so a caller passing the wrong day reads 0.
contract MockVwapSource is IVwapSource {
    mapping(uint32 fromUtcDay => uint256) public priceFrom;
    bool public reverts;

    function set(uint32 fromUtcDay, uint256 price) external {
        priceFrom[fromUtcDay] = price;
    }

    function setReverts(bool reverts_) external {
        reverts = reverts_;
    }

    function maxUtcDayVwapSince(uint16, uint32 fromUtcDay) external view returns (uint256) {
        require(!reverts, "source down");
        return priceFrom[fromUtcDay];
    }
}
