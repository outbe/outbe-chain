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
    /// @notice One successful settlement. `interestPaidMinor` is this payment's interest.
    ///         The Credis's `interestPaidMinor` is the sum of these payments
    ///         across successful settlements. A reverted settlement emits nothing
    ///         and leaves that total unchanged.
    event SettlementApplied(
        uint256 indexed credisId,
        uint256 interestPaidMinor,
        uint256 principalPaidMinor,
        uint256 gratisReturnedMinor,
        uint256 outstandingPrincipalMinor
    );
    event CredisSettled(uint256 indexed credisId);

    /// Use the reservation's pledge and every term it fixed, the call terms included.
    /// The caller must be its CCA.
    /// msg.value exactly matches reserved Gratis collateral in native COEN units.
    function issueCredis(uint256 reservationId) external payable returns (uint256 credisId, uint256 principalMinor);
    /// Any payer may repay. Freed collateral returns to the source's liquid Gratis.
    function settleCredis(uint256 credisId, uint256 amountMinor)
        external
        returns (uint256 principalPaidMinor, uint256 interestPaidMinor);
    /// What paying the Credis off now takes: its outstanding principal plus the interest
    /// accrued since the last settlement. Zero once it is Settled or Forfeited.
    function quoteSettlement(uint256 credisId) external view returns (uint256 payoffMinor);
    /// The owner gives up an Issued or Called Credis: its remaining Gratis burns, the principal is written off.
    function forfeitCredis(uint256 credisId)
        external
        returns (uint256 gratisBurnedMinor, uint256 principalWrittenOffMinor);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
