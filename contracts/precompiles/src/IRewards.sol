// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IRewards
/// @notice Validator reward Gem delivery events at 0x000000000000000000000000000000000000EE03.
/// @dev Rewards exposes no callable methods; these events are its whole outbound surface.
interface IRewards {
    /// @notice A prepared reward batch was delivered, at `entryPrice`.
    /// @param rewardUtcDay The UTC day the batch rewards.
    /// @param entryPrice COEN price in the batch's reference currency, six decimals.
    /// @param source 1 = the reward day's own VWAP, 2 = an earlier day's VWAP, 3 = the live quote.
    /// @param sourceDay UTC day `entryPrice` was read from; zero when `source` is the live quote.
    event RewardGemBatchPriced(uint32 indexed rewardUtcDay, uint256 entryPrice, uint8 source, uint32 sourceDay);
}
