// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/**
 * @title IPromisLimit
 * @notice Interface for the PromisLimit precompile / mock.
 * @dev Mirrors the `x/promislimit` Cosmos module keeper API.
 *      Tracks unallocated Promis capacity not consumed by daily auctions.
 *      The real implementation will be a stateful precompile at a fixed address;
 *      this interface is shared between the mock and the future precompile.
 */
interface IPromisLimit {
    /// @notice Emitted when unallocated Promis limit is increased
    /// @param amount Amount added (Promis, 18 decimals)
    /// @param newTotal New total unallocated Promis limit
    event UnallocatedPromisLimitAdded(uint256 amount, uint256 newTotal);

    /// @notice Add to the running total of unallocated Promis limit.
    ///         Called by Telosis after clearing when supply > issuedIntexCount.
    /// @param amount Unallocated Promis amount to add (scaled, 18 decimals)
    function addUnallocatedPromisLimit(uint256 amount) external;

    /// @notice Get the current total unallocated Promis limit
    /// @return Current total (Promis, 18 decimals)
    function totalUnallocatedPromisLimit() external view returns (uint256);
}
