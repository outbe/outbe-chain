// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IEmission
/// @notice Canonical EmissionDay ledger precompile at 0x000000000000000000000000000000000000100D.
interface IEmission {
    error EmissionDayNotFound();
    error InvalidEmissionDay(uint32 day);
    error ConflictingWwdLink(uint32 day, uint32 existingWwd, uint32 attemptedWwd);
    error DayAlreadyAllocated(uint32 day, uint8 state);
    error InvalidStateTransition(uint32 day, uint8 fromState, uint8 toState);
    error InvalidMetadosisStateTransition(uint32 day, uint8 fromState, uint8 toState);
    error AmountOverflow();
    error ConservationInvariantViolated(uint32 day, uint128 expectedLimit, uint128 landedSum);
    error WaitingWindowOverflow();
    error DateStep(uint8 kind, uint32 dateKey);

    struct EmissionDayData {
        uint32 day;
        uint32 wwd;
        uint64 metadosisStart;
        uint8 state;
        uint8 metadosisState;
        uint128 emissionLimit;
        uint128 metadosisLimit;
        uint128 lysisLimit;
        uint128 desisLimit;
        uint128 validators;
        uint128 waa;
        uint128 sra;
        uint128 cca;
        uint128 nods;
        uint128 promisLimit;
    }

    function getEmissionDay(uint32 day) external view returns (EmissionDayData memory);
}
