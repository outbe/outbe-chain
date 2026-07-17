// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @notice Minimal interface for the IntexFactory precompile call used by OriginRouter.
interface IIntexFactory {
    /// @notice Pay the native auction proceeds for a series to its creators.
    function distribute(uint32 worldwideDay) external payable;
}
