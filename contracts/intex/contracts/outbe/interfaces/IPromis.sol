// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title IPromis
/// @notice Soulbound Promis ERC20: only the trusted minter mints; transfers reverted.
interface IPromis {
    event Minted(address indexed to, uint256 amount);

    error TransfersDisabled();
    error ZeroAddress(string field);

    /// @notice Mint `amount` Promis to `holder`.
    function minePromis(address holder, uint256 amount) external;
}
