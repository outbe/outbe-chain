// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @notice Minimal interface for the IntexFactory precompile call used by OriginRouter.
interface IIntexFactory {
    /// @notice Burn native auction proceeds and credit them to the Promis limit pool in whole units.
    /// @param worldwideDay Worldwide day (yyyymmdd) the proceeds belong to.
    /// @param srcChainId Target chain the proceeds arrived from.
    function distribute(uint32 worldwideDay, uint32 srcChainId) external payable;
}
