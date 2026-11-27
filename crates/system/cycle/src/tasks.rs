//! Code-defined task registry.
//!
//! The code declares tasks as `const` data. There is no on-chain
//! registration path. Adding, removing, or re-parameterizing a task
//! is a hard-fork-coordinated code change. The dispatcher in
//! [`crate::runtime`] iterates this slice on every block. It fires each
//! task that has reached its next slot.

use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::errors::CycleError;

/// Stable identifier for Cycle tail-worker failure events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CycleWorkerId {
    AgentRewards = 1,
    DesisCleanup = 2,
}

impl CycleWorkerId {
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Stable on-chain identifier for each task. The module emits it as the indexed
/// `task` of [`crate::ICycle::CycleTaskCompleted`] and persists it in the
/// [`crate::schema::Cycle`] mappings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TaskId {
    ProcessEmissionDay = 0,
    ScheduleIntexCallPass = 1,
    AdvanceAuctions = 2,
    ScheduleGemCallPass = 3,
    CompleteAuctions = 4,
    ScheduleCredisCallPass = 5,
    ScheduleNodCallPass = 6,
}

impl TaskId {
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

/// One task's static configuration plus its handler function
/// pointer. Handlers receive only typed, read-only body capabilities.
#[derive(Clone, Copy)]
pub struct TaskSpec {
    pub id: u32,
    pub label: &'static str,
    /// Slot length in seconds. `period_seconds = 86_400` => daily.
    /// [`Self::new`] rejects zero. Callers read the value through [`Self::period_seconds`].
    period_seconds: u64,
    /// Nonzero gross handler allowance, in the enclosing meter's `u64` gas units.
    /// Private so all values pass the constructor's configuration validation.
    work_budget_gas: u64,
    /// First schedule anchor, in seconds after unix epoch zero.
    /// A value greater than or equal to the period stays valid.
    /// `period_seconds = 86_400, start_offset_seconds = 0` => UTC
    /// midnight slots; `period_seconds = 3600, start_offset_seconds =
    /// 1800` => "every hour at :30".
    pub start_offset_seconds: u64,
    /// When `true`, the dispatcher must also verify that V2 Phase 1
    /// (`CertifiedParentAccounting`) committed progress for the parent
    /// block before it fires the handler. That is,
    /// `last_accounted_block_number >= window.end_inclusive`. When `false`
    /// (e.g., a job that does NOT depend on parent accounting state), the
    /// handler runs on its own schedule. It does not consult
    /// [`outbe_system_common::accounting_progress::AccountingProgressView`].
    /// Default for the canonical protocol-cycle task is `true`.
    pub requires_accounting_window: bool,
    /// When `true` a backlog collapses to the latest due slot instead of
    /// replaying every missed one. ProcessEmissionDay owns its UTC-day decision,
    /// so replaying every missed hourly slot would duplicate orchestration.
    pub coalesces_backlog: bool,
    /// Recover a handler panic locally instead of failing the block.
    /// False for ProcessEmissionDay; true for the economic tasks.
    pub isolates_panic: bool,
    /// Tasks whose failure in the current walk blocks this task.
    /// Empty means the task is independent. The dispatcher reads this
    /// only after the task is due.
    pub depends_on: &'static [TaskId],
    /// Handler invoked when a slot fires. A revert rolls back the handler
    /// checkpoint. The dispatcher then records the slot. Any other error
    /// leaves `last_executed_at` unchanged.
    pub handler: TaskHandler,
}

/// What a task answers when it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    Done,
    /// The task changed nothing; the runner keeps its slot and runs it in the next block.
    NotReady,
}

#[derive(Clone, Copy)]
pub enum TaskHandler {
    #[cfg(test)]
    Test(fn(&BlockRuntimeContext<'_>) -> Result<()>),
    ProcessEmissionDay,
    ScheduleIntexCallPass,
    AdvanceAuctions,
    ScheduleGemCallPass,
    CompleteAuctions,
    ScheduleCredisCallPass,
    ScheduleNodCallPass,
}

impl TaskSpec {
    /// Builds a schedule with a required nonzero gross work allowance.
    /// Cadence is `(period_seconds, start_offset_seconds)`, both in UTC seconds.
    /// Zero periods and zero work allowances are rejected.
    /// `start_offset_seconds >= period_seconds` stays valid.
    /// The behavior flags start false. `depends_on` starts empty.
    /// The task table and fixtures set them.
    pub const fn new(
        id: u32,
        label: &'static str,
        (period_seconds, start_offset_seconds): (u64, u64),
        work_budget_gas: u64,
        handler: TaskHandler,
    ) -> core::result::Result<Self, CycleError> {
        if period_seconds == 0 {
            return Err(CycleError::ZeroTaskPeriod);
        }
        if work_budget_gas == 0 {
            return Err(CycleError::ZeroTaskWorkBudget);
        }
        Ok(Self {
            id,
            label,
            period_seconds,
            work_budget_gas,
            start_offset_seconds,
            requires_accounting_window: false,
            coalesces_backlog: false,
            isolates_panic: false,
            depends_on: &[],
            handler,
        })
    }

    /// Slot length in seconds. The value is greater than zero.
    pub const fn period_seconds(self) -> u64 {
        self.period_seconds
    }

    /// Gross handler gas allowance. The value is greater than zero.
    pub const fn work_budget_gas(self) -> u64 {
        self.work_budget_gas
    }
}

impl TaskHandler {
    pub(crate) fn run(self, ctx: &BlockRuntimeContext) -> Result<Readiness> {
        let done = match self {
            #[cfg(test)]
            Self::Test(handler) => handler(ctx),
            Self::ProcessEmissionDay => return crate::handler::process_emission_day(ctx),
            Self::ScheduleIntexCallPass => outbe_intexfactory::call::schedule_call_pass(ctx),
            Self::AdvanceAuctions => outbe_desis::advance_auctions(ctx),
            Self::ScheduleGemCallPass => outbe_gemfactory::call::schedule_call_pass(ctx),
            Self::CompleteAuctions => outbe_desis::complete_auctions(ctx),
            Self::ScheduleCredisCallPass => outbe_credisfactory::call::schedule_call_pass(ctx),
            Self::ScheduleNodCallPass => outbe_nodfactory::call::schedule_call_pass(ctx),
        };
        done.map(|()| Readiness::Done)
    }
}

/// Each tick advances every scheduled day that has a due step. So the days due at the
/// same midnight all move on the first tick after it. A brief only becomes a live auction
/// on this tick: a slower cadence would spend the commit window on waiting for the start.
/// `complete_auctions` uses the same cadence.
const ADVANCE_AUCTIONS_PERIOD_SECONDS: u64 = 3_600;

/// Every right's call sweep schedules a closed day, once a day.
const CALL_SWEEP_PERIOD_SECONDS: u64 = outbe_primitives::time::SECONDS_PER_DAY;

/// Const `?` is unstable on this toolchain (rust-lang/rust#143874).
/// The expansion returns the constructor error from the const function.
macro_rules! const_spec {
    ($spec:expr) => {
        match $spec {
            Ok(spec) => spec,
            Err(error) => return Err(error),
        }
    };
}

/// Hourly cadence of `process_emission_day`.
pub const PROCESS_EMISSION_DAY_PERIOD_SECONDS: u64 = 3_600;
/// `process_emission_day` runs from one second into each hour, so Metadosis of E runs in the
/// slot at 12:00:01 of E+1. The auction tasks share the offset to land in the slot of the brief.
pub const PROCESS_EMISSION_DAY_START_OFFSET_SECONDS: u64 = 1;

/// Agent rewards: 16,505,700 measured storage gas for 256 accrual and 3x256 cleanup
/// entries, multiplied by four and rounded up to 1,000 gas.
pub const AGENT_REWARDS_WORKER_BUDGET_GAS: u64 = 66_023_000;
/// Desis cleanup: 2,880,000 worst-case gas for four headers and 256 bid clears,
/// multiplied by four and rounded up to 1,000 gas.
pub const DESIS_CLEANUP_WORKER_BUDGET_GAS: u64 = 11_520_000;

/// Active task table in permanent numeric-id order.
/// The dispatcher walks this array in order inside one block.
/// A handler revert records the slot and continues the walk.
/// Any other handler error stops the walk.
/// A due task whose dependency failed in this walk records its slot
/// and does not run.
/// AdvanceAuctions must stay after ProcessEmissionDay.
/// Same-slot brief and auction order depends on that sequence.
///
/// Budgets use measured successful gross maxima from
/// `crates/blockchain/evm/src/executor/tests/receipts_and_gas/calibration.rs`, multiplied by four
/// and rounded up to 1,000 gas. In stable-id order those maxima are
/// [401900, 50300, 991492, 50300, 2583089, 50300, 50300].
/// The supported nested bridge profile performs calldata copy/hash, persistent
/// reads/writes, logs, and a real relay call. It is not a universal contract bound.
/// `advance_auctions` takes the step bounds of Desis, measured on the deployed bridge.
pub const fn active_tasks() -> core::result::Result<[TaskSpec; 7], CycleError> {
    Ok([
        // ProcessEmissionDay settles a contiguous completed UTC day. It MUST
        // observe the parent block's Phase 1 accounting before it fires.
        // Otherwise validator-pool top-ups and daily-fee reads would
        // race the parent-finalization tx.
        // The handler owns the calendar decision, so missed hourly slots
        // collapse to one execution at the latest due boundary.
        {
            let mut spec = const_spec!(TaskSpec::new(
                TaskId::ProcessEmissionDay.as_u32(),
                "process_emission_day",
                (
                    PROCESS_EMISSION_DAY_PERIOD_SECONDS,
                    PROCESS_EMISSION_DAY_START_OFFSET_SECONDS
                ),
                1_608_000,
                TaskHandler::ProcessEmissionDay,
            ));
            spec.requires_accounting_window = true;
            spec.coalesces_backlog = true;
            spec
        },
        // Reads finalized oracle VWAP history to call series. It has no
        // dependency on the parent block's settlement accounting.
        // The sweeps take their day from the block clock, so a missed slot
        // would only walk the same day again.
        {
            let mut spec = const_spec!(TaskSpec::new(
                TaskId::ScheduleIntexCallPass.as_u32(),
                "schedule_intex_call_pass",
                (CALL_SWEEP_PERIOD_SECONDS, 0),
                202_000,
                TaskHandler::ScheduleIntexCallPass,
            ));
            spec.requires_accounting_window = false;
            spec.coalesces_backlog = true;
            spec.isolates_panic = true;
            spec
        },
        // Gated like `process_emission_day`, so the brief it writes and this start
        // land in the same slot.
        // A poll, not a calendar slot. The handler reads the block clock and
        // sweeps every scheduled day. So replaying a missed slot does nothing
        // that the next firing would not already do.
        // The handler stops before a day whose due steps the rest cannot pay for,
        // and the next run starts with that day.
        {
            let mut spec = const_spec!(TaskSpec::new(
                TaskId::AdvanceAuctions.as_u32(),
                "advance_auctions",
                (
                    ADVANCE_AUCTIONS_PERIOD_SECONDS,
                    PROCESS_EMISSION_DAY_START_OFFSET_SECONDS
                ),
                outbe_desis::constants::ADVANCE_AUCTIONS_BUDGET_GAS,
                TaskHandler::AdvanceAuctions,
            ));
            spec.requires_accounting_window = true;
            spec.coalesces_backlog = true;
            spec.isolates_panic = true;
            spec.depends_on = &[TaskId::ProcessEmissionDay];
            spec
        },
        // Reads finalized oracle VWAP history to force-call gems. It has
        // no dependency on the parent block's settlement accounting.
        // The sweeps take their day from the block clock, so a missed slot
        // would only walk the same day again.
        {
            let mut spec = const_spec!(TaskSpec::new(
                TaskId::ScheduleGemCallPass.as_u32(),
                "schedule_gem_call_pass",
                (CALL_SWEEP_PERIOD_SECONDS, 0),
                202_000,
                TaskHandler::ScheduleGemCallPass,
            ));
            spec.requires_accounting_window = false;
            spec.coalesces_backlog = true;
            spec.isolates_panic = true;
            spec
        },
        // Polls the fan-in gate that `advance_auctions` arms, on the same cadence.
        // Each firing completes one waiting day.
        // Clears from bids already ingested and the router's frozen target
        // list. It has no dependency on the parent block's settlement accounting.
        // A poll has nothing to replay: a gap collapses to one clearing sweep.
        {
            let mut spec = const_spec!(TaskSpec::new(
                TaskId::CompleteAuctions.as_u32(),
                "complete_auctions",
                (
                    ADVANCE_AUCTIONS_PERIOD_SECONDS,
                    PROCESS_EMISSION_DAY_START_OFFSET_SECONDS
                ),
                10_333_000,
                TaskHandler::CompleteAuctions,
            ));
            spec.requires_accounting_window = false;
            spec.coalesces_backlog = true;
            spec.isolates_panic = true;
            spec.depends_on = &[TaskId::AdvanceAuctions];
            spec
        },
        // Reads finalized oracle VWAP history to call credis
        // positions. It has no dependency on the parent block's settlement
        // accounting.
        // The sweep takes its day from the block clock, so a missed slot
        // would only schedule the same day again.
        {
            let mut spec = const_spec!(TaskSpec::new(
                TaskId::ScheduleCredisCallPass.as_u32(),
                "schedule_credis_call_pass",
                (CALL_SWEEP_PERIOD_SECONDS, 0),
                202_000,
                TaskHandler::ScheduleCredisCallPass,
            ));
            spec.requires_accounting_window = false;
            spec.coalesces_backlog = true;
            spec.isolates_panic = true;
            spec
        },
        // Schedules the latest completed UTC day for the call sweep.
        // Missed slots would schedule the same day against the current clock.
        {
            let mut spec = const_spec!(TaskSpec::new(
                TaskId::ScheduleNodCallPass.as_u32(),
                "schedule_nod_call_pass",
                (CALL_SWEEP_PERIOD_SECONDS, 0),
                202_000,
                TaskHandler::ScheduleNodCallPass,
            ));
            spec.requires_accounting_window = false;
            spec.coalesces_backlog = true;
            spec.isolates_panic = true;
            spec
        },
    ])
}

pub const ACTIVE_TASK_ARRAY: [TaskSpec; 7] = match active_tasks() {
    Ok(specs) => specs,
    Err(_) => panic!("task periods and calibrated work budgets must be nonzero"),
};
pub const ACTIVE_TASKS: &[TaskSpec] = &ACTIVE_TASK_ARRAY;

/// Returns the next slot strictly greater than `last_executed_at`.
/// A zero period returns [`CycleError::ZeroTaskPeriod`].
/// Pure function: same `(period, offset, last)` tuple always returns
/// the same slot. Monotonically non-decreasing in `last_executed_at`
/// (and strictly increasing for `last_executed_at` past the first
/// slot).
///
/// Invariants of `Ok`:
/// * result `>= offset`,
/// * `(result - offset) % period == 0`,
/// * `result > last_executed_at`.
pub const fn next_fire_at(
    period: u64,
    offset: u64,
    last_executed_at: u64,
) -> core::result::Result<u64, CycleError> {
    if period == 0 {
        return Err(CycleError::ZeroTaskPeriod);
    }
    if last_executed_at < offset {
        return Ok(offset);
    }
    let diff = last_executed_at - offset;
    let Some(k) = (diff / period).checked_add(1) else {
        return Err(schedule_overflow(period, offset, last_executed_at));
    };
    let Some(span) = k.checked_mul(period) else {
        return Err(schedule_overflow(period, offset, last_executed_at));
    };
    match offset.checked_add(span) {
        Some(slot) => Ok(slot),
        None => Err(schedule_overflow(period, offset, last_executed_at)),
    }
}

const fn schedule_overflow(period: u64, offset: u64, timestamp: u64) -> CycleError {
    CycleError::ScheduleArithmeticOverflow {
        period,
        offset,
        timestamp,
    }
}

/// Returns the latest slot at or before `block_ts`. Recording this instead of
/// the first due slot collapses a backlog for a poll-style task. The next
/// call then sees a slot in the future and stops re-firing.
/// A zero period returns [`CycleError::ZeroTaskPeriod`].
/// A positive period cannot overflow `u64` when `block_ts >= offset`.
pub const fn last_fire_at(
    period: u64,
    offset: u64,
    block_ts: u64,
) -> core::result::Result<u64, CycleError> {
    if period == 0 {
        return Err(CycleError::ZeroTaskPeriod);
    }
    if block_ts < offset {
        return Ok(offset);
    }
    Ok(offset + (block_ts - offset) / period * period)
}

#[cfg(test)]
mod protocol_parameter_tests {
    use super::*;

    #[test]
    fn cycle_ids_match_the_abi_task_enum_and_the_completed_event() {
        use crate::ICycle::{CycleTask, CycleTaskCompleted};
        use alloy_sol_types::SolEvent;

        assert_eq!(CycleWorkerId::AgentRewards.as_u8(), 1);
        assert_eq!(CycleWorkerId::DesisCleanup.as_u8(), 2);
        for (id, task) in [
            (TaskId::ProcessEmissionDay, CycleTask::ProcessEmissionDay),
            (
                TaskId::ScheduleIntexCallPass,
                CycleTask::ScheduleIntexCallPass,
            ),
            (TaskId::AdvanceAuctions, CycleTask::AdvanceAuctions),
            (TaskId::ScheduleGemCallPass, CycleTask::ScheduleGemCallPass),
            (TaskId::CompleteAuctions, CycleTask::CompleteAuctions),
            (
                TaskId::ScheduleCredisCallPass,
                CycleTask::ScheduleCredisCallPass,
            ),
            (TaskId::ScheduleNodCallPass, CycleTask::ScheduleNodCallPass),
        ] {
            assert_eq!(id.as_u32(), u32::from(task as u8));
        }
        assert!(CycleTask::try_from(7u8).is_err());
        let log = CycleTaskCompleted {
            task: CycleTask::ScheduleGemCallPass,
        }
        .encode_log_data();
        assert_eq!(CycleTaskCompleted::SIGNATURE, "CycleTaskCompleted(uint8)");
        assert_eq!(log.topics().len(), 2);
        assert_eq!(log.topics()[0], CycleTaskCompleted::SIGNATURE_HASH);
        assert_eq!(
            log.topics()[1],
            alloy_primitives::B256::from(alloy_primitives::U256::from(3))
        );
        assert!(log.data.is_empty());
    }

    #[test]
    fn only_economic_cycle_tasks_enable_panic_isolation_and_worker_budgets_follow_headroom_rule() {
        assert!(!ACTIVE_TASKS[0].isolates_panic);
        assert!(ACTIVE_TASKS[1..].iter().all(|spec| spec.isolates_panic));
        assert!(
            !TaskSpec::new(9, "t", (60, 0), 1_000, TaskHandler::ScheduleNodCallPass)
                .unwrap()
                .isolates_panic
        );
        assert_eq!(AGENT_REWARDS_WORKER_BUDGET_GAS, 66_023_000);
        assert_eq!(DESIS_CLEANUP_WORKER_BUDGET_GAS, 11_520_000);
        for (budget, measured) in [
            (AGENT_REWARDS_WORKER_BUDGET_GAS, 16_505_700u64),
            (DESIS_CLEANUP_WORKER_BUDGET_GAS, 2_880_000),
        ] {
            assert_eq!(budget, (measured * 4).div_ceil(1_000) * 1_000);
        }
    }

    #[test]
    fn calibrated_budgets_are_nonzero_and_follow_checked_headroom_rule() {
        let maxima = [
            401_900u64, 50_300, 991_492, 50_300, 2_583_089, 50_300, 50_300,
        ];
        let tasks = active_tasks().expect("calibrated task table");
        let mut total = 0u64;
        for (spec, measured) in tasks.iter().zip(maxima) {
            let rounded = measured
                .checked_mul(4)
                .and_then(|gas| gas.checked_add(999))
                .map(|gas| gas / 1_000)
                .and_then(|units| units.checked_mul(1_000))
                .expect("measured handler headroom fits u64");
            assert!(spec.work_budget_gas() > 0);
            if spec.id != TaskId::AdvanceAuctions.as_u32() {
                assert_eq!(spec.work_budget_gas(), rounded, "task {}", spec.id);
            }
            total = total
                .checked_add(spec.work_budget_gas())
                .expect("combined allowances fit u64");
        }
        assert_eq!(total, 22_175_000);
    }
}
