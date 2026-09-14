// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title ICca - Checkout Credis Agent interface.
interface ICca {
    enum State {
        Unknown,
        Active,
        Deregistering,
        Deregistered
    }

    struct Cca {
        address cca;
        State state;
        uint256 bondedAmount;
        uint64 unbondUnlockAfter;
        uint256 rewardAmount;
    }

    event Bonded(address indexed cca, uint256 amount, uint256 selfBond, State state);
    event UnbondRequested(address indexed cca, uint256 amount, uint64 completeTime);
    event UnbondClaimed(address indexed cca, uint256 amount);
    event RewardAccrued(address indexed cca, uint32 indexed worldwideDay, uint256 amount);
    event RewardsClaimed(address indexed cca, uint256 amount);

    /// @notice Add msg.value to the caller's own bond; activation requires 1 billion COEN.
    function bond() external payable;
    /// @notice Enter Deregistering, freeze origination, and lock the bond for 128 days.
    function unbond() external;
    function claimUnbonded() external;
    /// @notice Claim all accrued native COEN, including while deregistering or after deregistration.
    function claimRewards() external;
    function getCca(address cca) external view returns (Cca memory);
    function getCcaState(address cca) external view returns (State);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
