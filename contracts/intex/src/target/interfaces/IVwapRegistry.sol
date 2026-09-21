// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IOriginRouter} from "../../origin/interfaces/IOriginRouter.sol";
import {IVwapSource} from "../../shared/interfaces/IVwapSource.sol";

/// @title IVwapRegistry
/// @author Outbe
/// @notice Target-chain record of the Oracle's finalized daily VWAPs, fed by the router from one message a day.
interface IVwapRegistry is IVwapSource {
    /// @notice One finalized day arrived: `rows` currencies were recorded for `utcDay`.
    event DailyVwapRecorded(uint32 indexed utcDay, uint256 rows);
    /// @notice The router allowed to record days changed.
    event RouterSet(address router);

    /// @notice Only the router records days.
    error NotRouter(address caller);
    /// @notice Zero address provided.
    /// @param field Field name that contains zero address.
    error ZeroAddress(string field);

    /// @notice Record the finalized VWAPs of `utcDay` (yyyymmdd). Days may arrive in any order; a repeat
    ///         overwrites with the value it already holds. Restricted to the router.
    function record(uint32 utcDay, IOriginRouter.DailyVwap[] calldata rows) external;
    /// @notice The finalized VWAP of COEN in `isoCode` on `utcDay`, 0 when it never arrived.
    function vwapOf(uint32 utcDay, uint16 isoCode) external view returns (uint64);
    /// @notice The newest day recorded for `isoCode`, 0 before the first.
    function lastUtcDay(uint16 isoCode) external view returns (uint32);
    /// @notice The router allowed to record days.
    function router() external view returns (address);
    /// @notice Set the router allowed to record days. Restricted to admin.
    function setRouter(address router_) external;
}
