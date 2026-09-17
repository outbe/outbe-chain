// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title ICredisFactory - credis lifecycle orchestrator.
interface ICredisFactory {
    event CredisIssued(uint256 indexed credisId, address indexed smartAccount, address indexed cca, uint256 amount);

    /// Consumes an unexpired private note and delivers its reserved principal.
    /// The encrypted authorization binds the destination SA. Both currency terms
    /// and entry price were fixed at quote time; policy rate is fixed at issuance.
    /// msg.value matches Gratis collateral in native-18 COEN and goes to the SA.
    function issueCredis(address ownerSA, bytes calldata encryptedUseAuth)
        external payable returns (uint256 credisId, uint256 amountStables);

    /// @notice Settle `amount` against a position and release the matching share of
    ///         collateral from the pledged lock ledger back to its balance.
    ///         A position is settleable from the moment it opens. Payment is applied
    ///         interest first, principal second, so an `amount` below the interest accrued
    ///         since the last settlement is rejected - query
    ///         `ICredis.accruedInterest` for that floor. Collateral is released in
    ///         proportion to the principal covered, and the settlement that clears
    ///         the last of the outstanding principal releases exactly the remainder,
    ///         leaving no dust.
    ///         When `amount` exceeds what the position still needs, only the required
    ///         part is pulled from the caller. Any caller may settle, including on
    ///         behalf of another account: the debt is pulled from the caller's own
    ///         balance while the freed collateral is always released to the original
    ///         pledger, so a payer can never redirect value to themselves.
    /// @return principal Principal covered by this settlement. Drives the collateral
    ///         released and the reduction in the position's outstanding balance.
    /// @return interest Accrued interest collected by this settlement. Taken in full
    ///         before any principal, and never carried between settlements.
    function settle(uint256 positionId, uint256 amount) external returns (uint256 principal, uint256 interest);

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
