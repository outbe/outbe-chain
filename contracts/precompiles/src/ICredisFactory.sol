// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ICredisFactory {
    error InvalidAmount();
    error CcaStakeMismatch();
    error ReservationNotFound();
    error ReservationExpired();
    error ReservationCcaMismatch();
    error ZeroIssueTerm();
    error InvalidSource();
    error InvalidOwner();
    error CredisAlreadyExists();
    error CredisClosed();
    error PaymentBelowAccruedInterest();
    error NotCalled();
    error CredisNotFound();
    error CredisNotIssued();
    error SettlementDeadlineNotPassed();
    error SettlementDeadlinePassed();
    error NothingOutstanding();
    error NotCredisOwner();
    error PayoffOverflow();
    error AssetDecimalsUndecodable();
    error AssetCurrencyUndecodable();
    error AssetMetadataChanged();
    error CoenStakeOverflow();
    error CredisChangedDuringIssuance();
    error CollateralIncreased();
    error CollateralReleaseMismatch();
    error CredisChangedDuringPayment();
    error ForfeitureCollateralMismatch();
    error CannotForfeitInIssueBlock();

    event CredisSeriesOpened(
        bytes14 indexed seriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint256 callPriceMinor,
        uint64 callWindow,
        uint64 callThreshold,
        uint64 callNoticePeriod,
        uint256 policyRate
    );
    event CredisIssued(
        uint256 indexed credisId,
        address indexed owner,
        bytes14 indexed seriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        address cca,
        address asset,
        uint256 principalMinor,
        uint256 gratisMinor
    );
    /// @notice One successful settlement. `interestSettledMinor` and `principalSettledMinor`
    ///         are this payment's interest and principal deltas, and `amountMinor` is their
    ///         sum. A reverted settlement emits nothing and leaves the Credis unchanged.
    event CredisSettled(
        uint256 indexed credisId,
        address indexed owner,
        address asset,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint256 amountMinor,
        uint256 interestSettledMinor,
        uint256 principalSettledMinor
    );

    /// Use the reservation's pledge and every term it fixed, the call terms included.
    /// The caller must be its CCA.
    /// msg.value exactly matches reserved Gratis collateral in native COEN units.
    function issueCredis(uint256 reservationId) external payable returns (uint256 credisId, uint256 principalMinor);
    /// Any payer may repay. Freed collateral is released to the source's liquid Gratis.
    function settleCredis(uint256 credisId, uint256 amountCapMinor)
        external
        returns (uint256 principalSettledMinor, uint256 interestSettledMinor);
    /// What paying the Credis off now takes: its outstanding principal plus the interest
    /// accrued since the last settlement. Zero once it is Settled or Forfeited. Interest
    /// grows by whole days, so a payment landing a day later needs that day more;
    /// `settleCredis` takes only what is owed, so paying more is safe.
    function quoteSettlement(uint256 credisId) external view returns (uint256 payoffMinor);

    /// @notice How close a right is to its group's call. The window ends at the last
    ///         finalized UTC day. Prices are COEN in `referenceCurrency` at scale 1e6.
    struct CallProgress {
        uint32 callWindowDays;
        uint32 callThresholdDays;
        /// Window days since the group's start, priced or not.
        uint32 daysChecked;
        /// Of them, days whose VWAP was above `callPriceMinor`.
        uint32 daysAboveCallPrice;
        /// The fewest further days above the call price before the call, as the oldest
        /// days leave the window. 0 once the call is decided.
        uint32 minDaysToCall;
        uint16 referenceCurrency;
        uint256 callPriceMinor;
        /// VWAP of the last finalized day, 0 when it has none.
        uint256 lastDayPriceMinor;
    }

    /// @notice Read the call progress of `credisId`. Reverts `CredisNotFound`, or
    ///         `CredisNotIssued` once it is Called, Settled or Forfeited.
    function getCallProgress(uint256 credisId) external view returns (CallProgress memory);
    /// The owner gives up an Issued or Called Credis before its settlement deadline: its remaining
    /// Gratis burns, the principal is forfeited and the Credis is deleted.
    function forfeitCredis(uint256 credisId)
        external
        returns (uint256 forfeitedGratisMinor, uint256 forfeitedPrincipalMinor);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
