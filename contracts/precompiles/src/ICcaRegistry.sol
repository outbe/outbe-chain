// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title ICcaRegistry - Checkout Credis Agent interface.
interface ICcaRegistry {
    enum State {
        Bonding,
        Active,
        Deregistering,
        Deregistered
    }

    struct Cca {
        address cca;
        string name;
        State state;
        uint256 bondedAmount;
        uint64 unbondUnlocksAfter;
        uint256 rewardAmount;
    }

    /// @notice Thrown when a CCA is not in Active standing.
    error CcaNotActive(address cca, State state);

    event Bonded(address indexed cca, uint256 amount, State state);
    event UnbondRequested(address indexed cca, uint256 amount, uint64 unbondUnlocksAfter);
    event UnbondClaimed(address indexed cca, uint256 amount);
    event RewardAccrued(address indexed cca, uint32 indexed utcDay, uint256 amount);
    event RewardsClaimed(address indexed cca, uint256 amount);

    /// @notice Bond COEN and set a nonempty CCA name.
    function bond(string calldata name) external payable;
    function unbond() external;
    function claimUnbonded() external;
    function claimRewards() external;
    function getCca(address cca) external view returns (Cca memory);
    function getCcaState(address cca) external view returns (State);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
