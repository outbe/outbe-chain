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

    event MetadosisLimitSet(uint32 indexed emissionDay, uint256 metadosisLimitMinor);

    event MetadosisCompleted(
        uint32 indexed emissionDay,
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

    event MetadosisCanceled(uint32 indexed emissionDay, uint8 reason);

    event MetadosisFailed(uint32 indexed emissionDay, uint8 reason);
}
