// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IEmission
/// @notice Canonical EmissionDay ledger precompile at 0x000000000000000000000000000000000000100D.
interface IEmission {
    error EmissionDayNotFound();
    error InvalidEmissionDay(uint32 day);
    error EmissionDayExists(uint32 day);
    error InvalidStateTransition(uint32 day, uint8 fromState, uint8 toState);
    error InvalidMetadosisStateTransition(uint32 day, uint8 fromState, uint8 toState);
    error InvalidDesisStateTransition(uint32 day, uint8 fromState, uint8 toState);
    error AmountOverflow();
    error WaitingWindowOverflow();
    error DateStep(uint8 kind, uint32 dateKey);

    /// The limits of the parts are set when the record is made, at 00:00 of the day before.
    /// `promisLimit` is the net amount the day sent to the Promis Limit and can be negative.
    struct EmissionDayData {
        uint32 day;
        uint32 wwd;
        uint64 metadosisStart;
        uint8 state;
        uint8 metadosisState;
        uint8 desisState;
        uint128 emissionLimit;
        uint128 validatorsLimit;
        uint128 ccaLimit;
        uint128 waaLimit;
        uint128 sraLimit;
        uint128 metadosisLimit;
        uint128 lysisLimit;
        uint128 desisLimit;
        uint128 nominalAmount;
        uint128 validators;
        uint128 waa;
        uint128 sra;
        uint128 cca;
        uint128 nods;
        uint128 intexes;
        int128 promisLimit;
    }

    function getEmissionDay(uint32 day) external view returns (EmissionDayData memory);
}
