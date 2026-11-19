// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {IERC20Metadata} from "@openzeppelin/contracts/token/ERC20/extensions/IERC20Metadata.sol";

/// @title IWCOEN
/// @notice WCOEN, the ERC-20 that wraps native COEN one to one.
interface IWCOEN is IERC20Metadata {
    event Deposit(address indexed dst, uint256 wad);
    event Withdrawal(address indexed src, uint256 wad);

    /// @notice Wrap the attached native COEN into WCOEN for the caller.
    function deposit() external payable;

    /// @notice Burn `wad` WCOEN of the caller and send it the same amount of native COEN.
    function withdraw(uint256 wad) external;

    /// @notice Raise the allowance of `guy` by `wad`.
    function increaseAllowance(address guy, uint256 wad) external returns (bool);

    /// @notice Lower the allowance of `guy` by `wad`.
    function decreaseAllowance(address guy, uint256 wad) external returns (bool);
}
