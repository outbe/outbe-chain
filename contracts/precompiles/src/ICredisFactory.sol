// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;
interface ICredisFactory {
    event CredisIssued(address indexed smartAccount, address indexed cca, uint256 amount);
    /// Consume a note for the complete stored reservation. The caller must be its CCA.
    /// msg.value exactly matches reserved Gratis collateral in native COEN units.
    function issueCredis(uint256 reservationId, bytes calldata proof) external payable
        returns (uint256 positionId, uint256 amountStables);
    /// Any payer may repay; freed collateral creates a note for the original source.
    function settle(uint256 positionId, uint256 amount) external returns (uint256 principal, uint256 interest);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
