// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IMetadosis {
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

    function getBootstrapEndTime() external view returns (uint64 endTime);

    function getEmissionDay(uint32 day)
        external
        view
        returns (
            uint32 emissionDay,
            uint256 emissionLimitMinor,
            uint32 wwd,
            uint64 metadosisStart,
            uint8 state
        );
}
