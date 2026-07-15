// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title ICredisFactory — credis lifecycle orchestrator.
interface ICredisFactory {
    event CredisRequested(address indexed bundleAccount, uint256 amount);

    /// @notice Open a credis position against a confidential Gratis pledge.
    ///         The bundle account presents `pledgeHandle` (the public id
    ///         returned by `pledgeGratis`) and `spendAuth` = HMAC(pledgeSecret,
    ///         "credis-bind" || bundleAccount), where the pledger EOA derived
    ///         `pledgeSecret` from its modify key + the handle off-chain. The
    ///         pledge is consumed once and bound to `bundleAccount`.
    /// @return positionId Derived from `pledgeHandle` and `bundleAccount`.
    /// @return amountStables Stablecoin amount disbursed (oracle-converted).
    function requestCredis(
        address asset,
        address bundleAccount,
        bytes32 pledgeHandle,
        bytes32 spendAuth
    ) external returns (uint256 positionId, uint256 amountStables);

    /// @notice Advance the named position by one anadosis installment and release
    ///         1/N of the pledged collateral back to the original pledger's
    ///         confidential Gratis balance. Caller MUST be the position's bundle
    ///         account.
    function anadosis(uint256 positionId) external;

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
