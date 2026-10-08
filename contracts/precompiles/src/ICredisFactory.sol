// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ICredisFactory {
    event CredisIssued(address indexed smartAccount, address indexed cca, uint256 principalMinor);
    /// A newer closed day replaced `skippedDay` while `inFlightDay` was still being swept.
    event SweepDaySkipped(uint8 indexed sweep, uint32 skippedDay, uint32 inFlightDay);
    /// A reference currency was left out of one day's Call scan because its window
    /// price could not be indexed. The next daily pass tries it again.
    event CallScanSkipped(uint16 indexed referenceCurrency, uint32 indexed utcDay);

    /// Consume a note for the complete stored reservation. The caller must be its CCA.
    /// msg.value exactly matches reserved Gratis collateral in native COEN units.
    function issueCredis(uint256 reservationId, bytes calldata proof)
        external
        payable
        returns (uint256 positionId, uint256 principalMinor);
    /// Any payer may repay. Freed collateral creates a note for the original source.
    function settleCredis(uint256 positionId, uint256 amountMinor)
        external
        returns (uint256 principalPaidMinor, uint256 interestMinor);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
