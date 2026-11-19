// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/// @title IWCOEN
/// @notice Wrapping of native COEN into the WCOEN ERC-20 and back.
interface IWCOEN {
    /// @notice Wrap the attached native COEN into WCOEN for the caller.
    function deposit() external payable;

    /// @notice Burn `wad` WCOEN of the caller and send it the same amount of native COEN.
    function withdraw(uint256 wad) external;
}
