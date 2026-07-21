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
    ///         pledge-lock ticket is consumed once and bound to `bundleAccount`,
    ///         crediting the collateral into the EOA's own pledged ledger.
    ///         The pledger EOA is NOT passed in calldata: the enclave reads it from
    ///         the ticket and returns it sealed, so it is stored as ciphertext on the
    ///         position (no EOA↔bundle linkage is visible to external observers).
    /// @return positionId Derived from `pledgeHandle` and `bundleAccount`.
    /// @return amountStables Stablecoin amount disbursed (oracle-converted).
    function requestCredis(address asset, address bundleAccount, bytes32 pledgeHandle, bytes32 spendAuth)
        external
        returns (uint256 positionId, uint256 amountStables);

    /// @notice Advance the named position by one anadosis installment and release
    ///         that installment's share of collateral from the pledger's own
    ///         confidential pledged ledger back to its balance. Caller MUST be the
    ///         position's bundle account. The pledger EOA is read from the stored
    ///         position, so it is not passed here.
    function anadosis(uint256 positionId) external;

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
