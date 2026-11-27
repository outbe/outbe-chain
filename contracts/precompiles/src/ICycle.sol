// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title ICycle
/// @notice Cycle dispatcher storage/events at 0x0000000000000000000000000000000000001010.
/// @dev The begin-zone CycleTick system transaction invokes Cycle. User calls do not.
interface ICycle {
    /// Caller-facing domain revert.
    error InvalidBlockUtcDay(uint32 active, uint32 blockDay);
    error ActiveUtcDayAfterBlock(uint32 active, uint32 blockDay);
    error BlockNumberRegression(uint64 current, uint64 attempted);
    error ZeroAccountingProgress();
    error AccountingProgressGap(uint64 current, uint64 attempted);
    error ZeroTaskPeriod();
    error ScheduleArithmeticOverflow(uint64 period, uint64 offset, uint64 timestamp);
    error DateStep(uint8 kind, uint32 dateKey);

    /// The tasks of the Cycle, in the order of their stable ids.
    enum CycleTask {
        ProcessEmissionDay,
        ScheduleIntexCallPass,
        AdvanceAuctions,
        ScheduleGemCallPass,
        CompleteAuctions,
        ScheduleCredisCallPass,
        ScheduleNodCallPass
    }

    /// Emitted once per slot when a task handler completes. A failed, over-budget or
    /// deferred task emits nothing: the task runs again later and the node log gives the cause.
    event CycleTaskCompleted(CycleTask indexed task);
}
