// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ICredisFactory {
    event CredisIssued(
        uint256 indexed credisId,
        address indexed owner,
        address indexed cca,
        address asset,
        uint256 principalMinor,
        uint256 gratisMinor
    );
    /// A newer closed day replaced `skippedDay` while `inFlightDay` was still being swept.
    event SweepDaySkipped(uint8 indexed sweep, uint32 skippedDay, uint32 inFlightDay);
    /// A reference currency was left out of one day's Call scan because its window
    /// price could not be indexed. The next daily pass tries it again.
    event CallScanSkipped(uint16 indexed referenceCurrency, uint32 indexed utcDay);
    /// A forfeit that failed left the Credis queued until `retryAt`.
    event ExpiryDeferred(uint256 indexed credisId, uint64 retryAt);

    /// Use the reservation's pledge and its quoted terms. The caller must be its CCA.
    /// `referenceCurrency` is elected here; the call anchor (previous closed UTC-day
    /// VWAP in it) and the issuance currency's policy rate are read at issuance.
    /// msg.value exactly matches reserved Gratis collateral in native COEN units.
    function issueCredis(uint256 reservationId, uint16 referenceCurrency)
        external
        payable
        returns (uint256 credisId, uint256 principalMinor);
    /// Any payer may repay. Freed collateral returns to the source's liquid Gratis.
    function settleCredis(uint256 credisId, uint256 amountMinor)
        external
        returns (uint256 principalPaidMinor, uint256 interestPaidMinor);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
