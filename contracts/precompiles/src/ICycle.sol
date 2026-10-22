// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title ICycle
/// @notice Cycle dispatcher storage/events at 0x0000000000000000000000000000000000001010.
/// @dev The begin-zone CycleTick system transaction invokes Cycle. User calls do not.
interface ICycle {
    /// Caller-facing domain revert.
    error InvalidBlockUtcDay(uint32 active, uint32 blockDay);
    error ActiveUtcDayAfterBlock(uint32 active, uint32 blockDay);
    error NativeValidatorBudgetOverflow();
    error SettlementBeforeParticipationWindowsClose();
    error DailySettledWithoutMetadosisReceipt();
    error MetadosisReceiptWithoutDailySettled();
    error MetadosisTerminalOverflow();
    error ValidatorTopupExceedsAllocation();
    error AllocationWithoutMetadosisReceipt();
    error AllocationDisagreesWithMetadosisReceipt();
    error ActiveUtcDayNotInitialized();
    error BlockNumberRegression(uint64 current, uint64 attempted);
    error ZeroTriggerPeriod();
    error ScheduleArithmeticOverflow(uint64 period, uint64 offset, uint64 timestamp);

    /// Emitted exactly once per slot when a trigger handler completes
    /// successfully. Indexed by the trigger's stable u32 id.
    event CycleTriggerExecuted(uint32 indexed id, uint64 scheduledAt, uint64 blockTimestamp, uint64 blockNumber);
}
