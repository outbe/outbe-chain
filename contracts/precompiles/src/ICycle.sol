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
    error BlockNumberRegression(uint64 current, uint64 attempted);
    error ZeroAccountingProgress();
    error AccountingProgressGap(uint64 current, uint64 attempted);
    error ZeroTriggerPeriod();
    error ScheduleArithmeticOverflow(uint64 period, uint64 offset, uint64 timestamp);
    error DateStep(uint8 kind, uint32 dateKey);

    /// Emitted exactly once per slot when a trigger handler completes
    /// successfully. Indexed by the trigger's stable u32 id.
    event CycleTriggerExecuted(uint32 indexed id, uint64 scheduledAt, uint64 blockTimestamp, uint64 blockNumber);

    /// Emitted when a due handler reverts. `scheduledAt` is the recorded slot.
    /// The event carries no error text. A skipped dependency does not emit it.
    event CycleTriggerFailed(uint32 indexed id, uint64 scheduledAt, uint64 blockTimestamp, uint64 blockNumber);

    /// Emitted after an over-budget handler has rolled back. The slot stays pending.
    event CycleTriggerOverBudget(uint32 indexed id, uint64 consumedGas, uint64 budgetGas, uint64 blockNumber);

    /// Unattempted pending work: reason 1 is capacity, reason 2 is a deferred dependency.
    /// `scheduledAt` is the slot that a successful attempt would have recorded.
    event CycleTriggerDeferred(uint32 indexed id, uint64 scheduledAt, uint64 blockNumber, uint8 reason);

    /// Emitted when daily emission limit allocation reverts and the emission limit
    /// goes to the Promis Limit once.
    event DayEmissionLimitAllocationFailed(uint32 indexed emissionDay, uint256 emissionLimitMinor, uint8 reason);
}
