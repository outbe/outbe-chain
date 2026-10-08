// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ICredisFactory {
    event CredisIssued(address indexed smartAccount, address indexed cca, uint256 principalMinor);
    /// Use the reservation's pledge for the stored terms. The caller must be its CCA.
    /// msg.value exactly matches reserved Gratis collateral in native COEN units.
    function issueCredis(uint256 reservationId) external payable returns (uint256 positionId, uint256 principalMinor);
    /// Any payer may repay. Freed collateral returns to the source's liquid Gratis.
    function settleCredis(uint256 positionId, uint256 amountMinor)
        external
        returns (uint256 principalPaidMinor, uint256 interestMinor);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
