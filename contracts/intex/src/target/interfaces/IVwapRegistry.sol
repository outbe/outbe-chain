// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IOriginRouter} from "../../origin/interfaces/IOriginRouter.sol";
import {IVwapSource} from "../../shared/interfaces/IVwapSource.sol";

/// @title IVwapRegistry
/// @author Outbe
/// @notice Target-chain record of the Oracle's finalized daily VWAPs, fed by the router.
interface IVwapRegistry is IVwapSource {
    event DailyVwapRecorded(uint32 indexed utcDay, uint256 rows);
    event RouterSet(address router);

    error NotRouter(address caller);
    error ZeroAddress(string field);

    /// @notice Days may arrive in any order.
    function record(uint32 utcDay, IOriginRouter.DailyVwap[] calldata rows) external;
    function vwapOf(uint32 utcDay, uint16 isoCode) external view returns (uint64);
    function lastUtcDay(uint16 isoCode) external view returns (uint32);
    function router() external view returns (address);
    function setRouter(address router_) external;
}
