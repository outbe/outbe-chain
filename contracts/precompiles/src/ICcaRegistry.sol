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
        /// @dev Native COEN, 18 decimals.
        uint256 bondedMinor;
        uint64 unbondUnlocksAfter;
    }

    /// @notice Thrown when a CCA is not in Active standing.
    error CcaNotActive(address cca, State state);

    /// @notice Bond and unbond amounts are native COEN, 18 decimals.
    event Bonded(address indexed cca, uint256 amountMinor, State state);
    event UnbondRequested(address indexed cca, uint256 amountMinor, uint64 unbondUnlocksAfter);
    event UnbondClaimed(address indexed cca, uint256 amountMinor);

    /// @notice Bond COEN and set a nonempty CCA name.
    function bond(string calldata name) external payable;
    function unbond() external;
    function claimUnbonded() external;
    function getCca(address cca) external view returns (Cca memory);
    function getCcaState(address cca) external view returns (State);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
