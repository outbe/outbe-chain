// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IOriginRouter} from "../../origin/interfaces/IOriginRouter.sol";
import {IVwapSource} from "../../shared/interfaces/IVwapSource.sol";

/// @title IVwapRegistry
/// @author Outbe
/// @notice Target-chain record of the Oracle's finalized daily VWAPs, fed by the router.
/// @dev Deployed on each target chain behind a UUPS proxy. Only the TargetRouter records days, one
///      DAILY_VWAP message at a time; the collection reads them through {IVwapSource} to render
///      Qualified. The origin chain has no registry: there the collection reads the IntexFactory.
interface IVwapRegistry is IVwapSource {
    /// @notice Emitted when a day's prices are recorded.
    /// @param utcDay UTC day (yyyymmdd).
    /// @param rows Prices the message carried.
    event DailyVwapRecorded(uint32 indexed utcDay, uint256 rows);

    /// @notice Emitted when the recording router is set.
    /// @param router The TargetRouter allowed to record.
    event RouterSet(address router);

    /// @notice The caller is not the recording router.
    error NotRouter(address caller);

    /// @notice A required address argument was zero.
    error ZeroAddress(string field);

    /// @notice Record one finalized day's prices. Router only, idempotent; days may arrive in any order.
    /// @param utcDay UTC day (yyyymmdd).
    /// @param rows One price per reference currency.
    function record(uint32 utcDay, IOriginRouter.DailyVwap[] calldata rows) external;

    /// @notice The recorded price of one day, or 0 when that day never arrived.
    /// @param utcDay UTC day (yyyymmdd).
    /// @param isoCode Reference currency.
    function vwapOf(uint32 utcDay, uint16 isoCode) external view returns (uint64);

    /// @notice Newest day recorded for `isoCode`, or 0 before the first one.
    function lastUtcDay(uint16 isoCode) external view returns (uint32);

    /// @notice The TargetRouter allowed to record.
    function router() external view returns (address);

    /// @notice Point the registry at another router. Admin only.
    /// @param router_ The TargetRouter allowed to record from now on.
    function setRouter(address router_) external;
}
