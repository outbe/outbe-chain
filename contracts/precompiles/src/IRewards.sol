// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IRewards
/// @notice Rewards precompile at 0x000000000000000000000000000000000000EE03
interface IRewards {
    /// Emitted when a validator claims their pending emission rewards.
    event RewardsClaimed(address indexed validator, uint256 amount);

    function claimRewards() external returns (uint256);
    function pendingRewards(address validator) external view returns (uint256);
}
