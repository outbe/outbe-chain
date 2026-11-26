// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IMetadosis {
    error VwapMustBeNonZero();
    error InvalidMetadosisLimitSplit(uint256 dayLimit, uint256 nodLoadsMinor);
    error DesisLimitUnavailable(uint256 desisLimitMinor, uint256 available);
    error FormedDayLimitCannotBeReplaced();
    error UnknownWorldwideDayStatus();
    error FormedDayLimitImmutable();
    error FormingWindowOverflow();
    error OfferingWindowOverflow();
    error DurationOverflow(string label);
    error UtcPlus14TimestampOverflow();
    error DateStep(uint8 kind, uint32 dateKey);

    event MetadosisLimitSet(uint32 indexed emissionDay, uint256 metadosisLimitMinor);

    event MetadosisCompleted(uint32 indexed emissionDay, uint256 nominalAmountMinor);

    event MetadosisCanceled(uint32 indexed emissionDay, uint8 reason);

    event MetadosisFailed(uint32 indexed emissionDay, uint8 reason);

    event MetadosisLimitExceeded(uint32 indexed emissionDay, uint256 excessMinor);
}
