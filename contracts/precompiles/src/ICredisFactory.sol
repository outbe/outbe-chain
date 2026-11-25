// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ICredisFactory {
    error InvalidAmount();
    error ReservationNotFound();
    error ReservationExpired();
    error ReservationOwnerMismatch();
    error ZeroIssueTerm();
    error InvalidPledgeAccount();
    error InvalidOwner();
    error CredisAlreadyExists();
    error CredisClosed();
    error PaymentBelowAccruedFee();
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
    error CredisChangedDuringIssuance();
    error CollateralIncreased();
    error CollateralReleaseMismatch();
    error CredisChangedDuringPayment();
    error ForfeitureCollateralMismatch();
    error CannotForfeitInIssueBlock();

    event CredisSeriesOpened(
        bytes14 indexed seriesId,
        uint8 subSeries,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint256 entryPriceMinor,
        uint256 callPriceMinor,
        uint64 callWindow,
        uint64 callThreshold,
        uint64 callNoticePeriod
    );
    /// Every unsettled Credis of the sub-series is called with its call group.
    event CredisSeriesCalled(
        bytes14 indexed seriesId,
        uint8 subSeries,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint64 calledAt,
        uint64 settlementDeadline
    );
    /// The deadline of the call group passed: the unsettled Credis of the sub-series are forfeited.
    event CredisSeriesClosed(
        bytes14 indexed seriesId,
        uint8 subSeries,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint32 forfeitedCount,
        uint256 forfeitedAmountMinor
    );
    /// @notice One successful settlement. `settledFeeMinor` and `settledAmountMinor`
    ///         are this payment's fee and amount deltas, and `amountMinor` is their
    ///         sum. A reverted settlement emits nothing and leaves the Credis unchanged.
    event CredisSettled(
        uint256 indexed credisId,
        address indexed owner,
        address asset,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint256 amountMinor,
        uint256 settledFeeMinor,
        uint256 settledAmountMinor
    );

    /// Issue the Credis of the reservation with every term it fixed, the call terms included.
    /// The caller must be its Credis owner and must have approved the amount of the asset to
    /// the VaultRouter. `pledge` is the authorization of the main account, sealed to the
    /// offer key: the main account and its MAC over the account, the Gratis quantity, the
    /// reservation id and the chain id.
    function issueCredis(uint256 credisReservationId, bytes calldata pledge)
        external
        returns (uint256 credisId, uint256 amountMinor);
    /// Any payer may repay. Freed collateral goes back to the free Gratis of the main account.
    /// The payer approves the VaultRouter for the repayment first.
    function settleCredis(uint256 credisId, uint256 amountCapMinor)
        external
        returns (uint256 settledAmountMinor, uint256 settledFeeMinor);
    /// What paying the Credis off now takes: its outstanding amount plus the fee
    /// accrued since the last settlement. Zero once it is Settled or Forfeited. Fee
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
    /// Gratis burns, the amount is forfeited and the Credis is deleted.
    function forfeitCredis(uint256 credisId)
        external
        returns (uint256 forfeitedGratisMinor, uint256 forfeitedAmountMinor);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
