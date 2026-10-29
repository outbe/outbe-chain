// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IMetadosis {
    error VwapMustBeNonZero();
    error InvalidLysisLimitSplit(uint256 dayLimit, uint256 lysisLimitMinor);
    error LysisLimitCreditMismatch();
    error DesisLimitUnavailable(uint256 desisLimitMinor, uint256 available);
    error FormedDayLimitCannotBeReplaced();
    error UnknownWorldwideDayStatus();
    error FormedDayLimitImmutable();
    error FormingWindowOverflow();
    error LookbackWindowOverflow();
    error OfferingWindowOverflow();
    error WaitingWindowOverflow();
    error DurationOverflow(string label);
    error UtcPlus14TimestampOverflow();
    error DateStep(uint8 kind, uint32 dateKey);

    event MetadosisAccumulation(
        uint32 indexed date, uint256 metadosisLimitMinor, uint256 totalAccumulatedMinor, uint64 blockNumber
    );

    event DayLimitFormed(
        uint32 indexed worldwideDay,
        uint256 baseLimitMinor,
        uint256 promisLimitBeforeMinor,
        uint256 promisLimitTakenMinor,
        uint256 promisLimitAfterMinor,
        uint256 metadosisLimitMinor,
        uint64 blockNumber
    );

    event MetadosisSkipped(uint32 indexed worldwideDay, string reason, string status, uint64 blockNumber);

    event MetadosisExecuted(
        uint32 indexed worldwideDay,
        uint256 tributeNominalTotalMinor,
        uint256 gratisDemandMinor,
        uint256 dayGratisLimitMinor,
        uint256 lysisLimitMinor,
        uint256 unusedLysisLimitMinor,
        uint256 lysisAllocationMinor,
        uint256 promisLimitReturnedMinor,
        string status,
        uint64 blockNumber
    );

    event MetadosisWorldwideDayProcessed(
        uint32 indexed worldwideDay,
        uint256 metadosisLimitMinor,
        uint256 promisLimitReturnedMinor,
        string status,
        string action
    );
}
