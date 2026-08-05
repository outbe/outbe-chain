// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title ICredisFactory — credis lifecycle orchestrator.
interface ICredisFactory {
    event CredisRequested(address indexed smartAccount, uint256 amount);

    /// @notice Open a credis position against a confidential Gratis pledge.
    ///         The smart account presents `pledgeHandle` (the public id
    ///         returned by `pledgeGratis`) and `spendAuth` = HMAC(pledgeSecret,
    ///         "credis-bind" || smartAccount), where the pledger EOA derived
    ///         `pledgeSecret` from its modify key + the handle off-chain. The
    ///         pledge-lock ticket is consumed once and bound to `smartAccount`.
    /// @return positionId Derived from `pledgeHandle` and `smartAccount`.
    /// @return amountStables Stablecoin amount disbursed (oracle-converted).
    function requestCredis(address asset, address smartAccount, bytes32 pledgeHandle, bytes32 spendAuth)
        external
        returns (uint256 positionId, uint256 amountStables);

    /// @notice Pay `amount` toward the position's unpaid anadosis schedule and
    ///         release the matching share of collateral from the pledged lock
    ///         ledger back to its balance. Installments are settled in order:
    ///         each is covered in full while the supplied amount lasts, and the
    ///         last one reached may be covered partially (its remainder stays on
    ///         `Anadosis.unpaidAmount`). When `amount` exceeds the position's
    ///         outstanding balance only the required part is pulled from the caller.
    ///         Caller MUST be the position's smart account.
    /// @return totalPaidAmount Stablecoin actually pulled — `min(amount, outstanding)`.
    function anadosis(uint256 positionId, uint256 amount) external returns (uint256 totalPaidAmount);

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
