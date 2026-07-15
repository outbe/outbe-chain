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
    ///         pledge is consumed once and bound to `bundleAccount`, and the
    ///         collateral is moved into the credis escrow.
    /// @param eoaAccount The original pledger EOA; checked against the pledge
    ///        record inside the enclave (its pledged ledger is debited).
    ///        TODO(privacy): pass this encrypted so observers can't read it.
    /// @return positionId Derived from `pledgeHandle` and `bundleAccount`.
    /// @return amountStables Stablecoin amount disbursed (oracle-converted).
    function requestCredis(
        address asset,
        address bundleAccount,
        address eoaAccount,
        bytes32 pledgeHandle,
        bytes32 spendAuth
    ) external returns (uint256 positionId, uint256 amountStables);

    /// @notice Advance the named position by one anadosis installment and release
    ///         1/N of the escrowed collateral back to the pledger's confidential
    ///         Gratis balance. Caller MUST be the position's bundle account.
    /// @param eoaAccount The pledger EOA to release the installment to; checked
    ///        against the pledge record inside the enclave.
    ///        TODO(privacy): pass this encrypted so observers can't read it.
    function anadosis(uint256 positionId, address eoaAccount) external;

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
