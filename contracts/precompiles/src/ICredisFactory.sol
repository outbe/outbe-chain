// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title ICredisFactory — credis lifecycle orchestrator.
interface ICredisFactory {
    event CredisRequested(address indexed smartAccount, uint256 amount);

    /// @notice Open a credis position against a confidential Gratis pledge.
    ///         The bundle account presents `pledgeHandle` (the public id
    ///         returned by `pledgeGratis`) and `spendAuth` = HMAC(pledgeSecret,
    ///         "credis-bind" || smartAccount), where the pledger EOA derived
    ///         `pledgeSecret` from its modify key + the handle off-chain. The
    ///         pledge-lock ticket is consumed once and bound to `smartAccount`.
    /// @return positionId Derived from `pledgeHandle` and `smartAccount`.
    /// @return amountStables Stablecoin amount disbursed (oracle-converted).
    function requestCredis(address asset, address smartAccount, bytes32 pledgeHandle, bytes32 spendAuth)
        external
        returns (uint256 positionId, uint256 amountStables);

    /// @notice Advance the named position by one anadosis payment and release
    ///         that installment's share of collateral from the pledged lock ledger
    ///         back to its balance. Caller MUST be the position's bundle account.
    function anadosis(uint256 positionId) external;

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
