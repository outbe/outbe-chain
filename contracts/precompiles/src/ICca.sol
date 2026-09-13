// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title ICca - bonded Checkout Credis Agents and daily origination rewards.
interface ICca {
    enum State {
        Unknown,
        Active,
        Suspended,
        Deregistered
    }

    struct Cca {
        State state;
        uint256 selfBond;
        uint256 unbondAmount;
        uint64 unbondCompleteTime;
        // Six-decimal opening GRATIS less GRATIS burned on void; repayment does not reduce it.
        uint256 rewardWeight;
        // Native COEN atomic units (18 decimals), separate from selfBond.
        uint256 claimableRewards;
    }

    event Bonded(address indexed cca, uint256 amount, uint256 selfBond, State state);
    event UnbondRequested(address indexed cca, uint256 amount, uint64 completeTime);
    event UnbondClaimed(address indexed cca, uint256 amount);
    event RewardWeightChanged(address indexed cca, uint256 weight);
    event RewardAccrued(address indexed cca, uint32 indexed worldwideDay, uint256 amount);
    event RewardsClaimed(address indexed cca, uint256 amount);

    /// @notice Add msg.value to the caller's own bond; activation requires 1 billion COEN.
    function bond() external payable;
    /// @notice Suspend origination and move the entire bond into a 128-day cooldown.
    function unbond() external;
    function claimUnbonded() external;
    /// @notice Claim all accrued native COEN, including after suspension or deregistration.
    function claimRewards() external;
    function getCca(address cca) external view returns (Cca memory);
    function getCcaState(address cca) external view returns (State);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
