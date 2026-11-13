//! Code-defined trigger registry.
//!
//! The code declares triggers as `const` data. There is no on-chain
//! registration path. Adding, removing, or re-parameterizing a trigger
//! is a hard-fork-coordinated code change. The dispatcher in
//! [`crate::runtime`] iterates this slice on every block. It fires each
//! trigger that has reached its next slot.

use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::errors::CycleError;

/// Stable on-chain identifier for each trigger. The module emits it as
/// the indexed `id` in [`crate::ICycle::CycleTriggerExecuted`] and
/// [`crate::ICycle::CycleTriggerFailed`], and persists it in the
/// [`crate::schema::Cycle`] mappings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TriggerId {
    ProcessEmissionDay = 0,
    ScheduleIntexCallPass = 1,
    AdvanceAuctions = 2,
    ScheduleGemCallPass = 3,
    CompleteAuctions = 4,
    ScheduleCredisCallPass = 5,
    ScheduleNodCallPass = 6,
}

impl TriggerId {
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

/// One trigger's static configuration plus its handler function
/// pointer. Handlers receive only typed, read-only body capabilities.
#[derive(Clone, Copy)]
pub struct TriggerSpec {
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
    /// Default for the canonical protocol-cycle trigger is `true`.
    pub requires_accounting_window: bool,
    /// When `true` a backlog collapses to the latest due slot instead of
    /// replaying every missed one. ProcessEmissionDay owns its UTC-day decision,
    /// so replaying every missed hourly slot would duplicate orchestration.
    pub coalesces_backlog: bool,
    /// Triggers whose failure in the current walk blocks this trigger.
    /// Empty means the trigger is independent. The dispatcher reads this
    /// only after the trigger is due.
    pub depends_on: &'static [TriggerId],
    /// Handler invoked when a slot fires. A revert rolls back the handler
    /// checkpoint. The dispatcher then records the slot. Any other error
    /// leaves `last_executed_at` unchanged.
    pub handler: TriggerHandler,
}

/// What a task answers when it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    Done,
    /// The task changed nothing; the runner keeps its slot and runs it in the next block.
    NotReady,
}

#[derive(Clone, Copy)]
pub enum TriggerHandler {
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

impl TriggerSpec {
    /// Builds a schedule with a required nonzero gross work allowance.
    /// Cadence is `(period_seconds, start_offset_seconds)`, both in UTC seconds.
    /// Zero periods and zero work allowances are rejected.
    /// `start_offset_seconds >= period_seconds` stays valid.
    /// The two behavior flags start false. `depends_on` starts empty.
    /// The trigger table and fixtures set them.
    pub const fn new(
        id: u32,
        label: &'static str,
        (period_seconds, start_offset_seconds): (u64, u64),
        work_budget_gas: u64,
        handler: TriggerHandler,
    ) -> core::result::Result<Self, CycleError> {
        if period_seconds == 0 {
            return Err(CycleError::ZeroTriggerPeriod);
        }
        if work_budget_gas == 0 {
            return Err(CycleError::ZeroTriggerWorkBudget);
        }
        Ok(Self {
            id,
            label,
            period_seconds,
            work_budget_gas,
            start_offset_seconds,
            requires_accounting_window: false,
            coalesces_backlog: false,
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

impl TriggerHandler {
    pub(crate) fn run(self, ctx: &BlockRuntimeContext) -> Result<Readiness> {
        let done = match self {
            #[cfg(test)]
            Self::Test(handler) => handler(ctx),
            Self::ProcessEmissionDay => return crate::handler::process_emission_day(ctx),
            Self::ScheduleIntexCallPass => outbe_intexfactory::called::schedule_call_pass(ctx),
            Self::AdvanceAuctions => outbe_desis::advance_auctions(ctx),
            Self::ScheduleGemCallPass => outbe_gemfactory::called::schedule_call_pass(ctx),
            Self::CompleteAuctions => outbe_desis::complete_auctions(ctx),
            Self::ScheduleCredisCallPass => outbe_credisfactory::called::schedule_call_pass(ctx),
            Self::ScheduleNodCallPass => outbe_nodfactory::called::schedule_call_pass(ctx),
        };
        done.map(|()| Readiness::Done)
    }
}

/// Each tick advances every scheduled day that has a due step. So the days due at the
/// same midnight all move on the first tick after it. A brief only becomes a live auction
/// on this tick: a slower cadence would spend the commit window on waiting for the start.
///
/// An e2e day is minutes long, so it advances faster still.
/// `complete_auctions` uses the same cadence.
#[cfg(not(feature = "e2e-test"))]
const ADVANCE_AUCTIONS_PERIOD_SECONDS: u64 = 3_600;
#[cfg(feature = "e2e-test")]
const ADVANCE_AUCTIONS_PERIOD_SECONDS: u64 = 60;

/// Every right's call sweep schedules a closed day, daily in production. An e2e run
/// seeds the days it reads instead of living through them, so it recurs sooner.
#[cfg(not(feature = "e2e-test"))]
const CALL_SWEEP_PERIOD_SECONDS: u64 = outbe_primitives::time::SECONDS_PER_DAY;
#[cfg(feature = "e2e-test")]
const CALL_SWEEP_PERIOD_SECONDS: u64 = 60;

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
/// `process_emission_day` runs one second into each hour, so Metadosis of E starts at 00:00:01 of E+1.
/// The auction tasks share the offset to land in the slot of the brief.
pub const PROCESS_EMISSION_DAY_START_OFFSET_SECONDS: u64 = 1;

/// Active trigger table in permanent numeric-id order.
/// The dispatcher walks this array in order inside one block.
/// A handler revert records the slot and continues the walk.
/// Any other handler error stops the walk.
/// A due trigger whose dependency failed in this walk records its slot
/// and does not run.
/// AdvanceAuctions must stay after ProcessEmissionDay.
/// Same-slot brief and auction order depends on that sequence.
///
/// Budgets use measured successful gross maxima from
/// `crates/blockchain/evm/src/executor/tests/receipts_and_gas/calibration.rs`, multiplied by four
/// and rounded up to 1,000 gas. In stable-id order those maxima are
/// [381500, 50300, 991492, 50300, 2583089, 50300, 50300].
/// The supported nested bridge profile performs calldata copy/hash, persistent
/// reads/writes, logs, and a real relay call. It is not a universal contract bound.
/// `advance_auctions` takes the step bounds of Desis, measured on the deployed bridge.
pub const fn active_triggers() -> core::result::Result<[TriggerSpec; 7], CycleError> {
    Ok([
        // ProcessEmissionDay settles a contiguous completed UTC day. It MUST
        // observe the parent block's Phase 1 accounting before it fires.
        // Otherwise validator-pool top-ups and daily-fee reads would
        // race the parent-finalization tx.
        // The handler owns the calendar decision, so missed hourly slots
        // collapse to one execution at the latest due boundary.
        {
            let mut spec = const_spec!(TriggerSpec::new(
                TriggerId::ProcessEmissionDay.as_u32(),
                "process_emission_day",
                (
                    PROCESS_EMISSION_DAY_PERIOD_SECONDS,
                    PROCESS_EMISSION_DAY_START_OFFSET_SECONDS
                ),
                1_526_000,
                TriggerHandler::ProcessEmissionDay,
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
            let mut spec = const_spec!(TriggerSpec::new(
                TriggerId::ScheduleIntexCallPass.as_u32(),
                "schedule_intex_call_pass",
                (CALL_SWEEP_PERIOD_SECONDS, 0),
                202_000,
                TriggerHandler::ScheduleIntexCallPass,
            ));
            spec.requires_accounting_window = false;
            spec.coalesces_backlog = true;
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
            let mut spec = const_spec!(TriggerSpec::new(
                TriggerId::AdvanceAuctions.as_u32(),
                "advance_auctions",
                (
                    ADVANCE_AUCTIONS_PERIOD_SECONDS,
                    PROCESS_EMISSION_DAY_START_OFFSET_SECONDS
                ),
                outbe_desis::constants::ADVANCE_AUCTIONS_BUDGET_GAS,
                TriggerHandler::AdvanceAuctions,
            ));
            spec.requires_accounting_window = true;
            spec.coalesces_backlog = true;
            spec.depends_on = &[TriggerId::ProcessEmissionDay];
            spec
        },
        // Reads finalized oracle VWAP history to force-call gems. It has
        // no dependency on the parent block's settlement accounting.
        // The sweeps take their day from the block clock, so a missed slot
        // would only walk the same day again.
        {
            let mut spec = const_spec!(TriggerSpec::new(
                TriggerId::ScheduleGemCallPass.as_u32(),
                "schedule_gem_call_pass",
                (CALL_SWEEP_PERIOD_SECONDS, 0),
                202_000,
                TriggerHandler::ScheduleGemCallPass,
            ));
            spec.requires_accounting_window = false;
            spec.coalesces_backlog = true;
            spec
        },
        // Polls the fan-in gate that `advance_auctions` arms, on the same cadence.
        // Each firing completes one waiting day.
        // Clears from bids already ingested and the router's frozen target
        // list. It has no dependency on the parent block's settlement accounting.
        // A poll has nothing to replay: a gap collapses to one clearing sweep.
        {
            let mut spec = const_spec!(TriggerSpec::new(
                TriggerId::CompleteAuctions.as_u32(),
                "complete_auctions",
                (
                    ADVANCE_AUCTIONS_PERIOD_SECONDS,
                    PROCESS_EMISSION_DAY_START_OFFSET_SECONDS
                ),
                10_333_000,
                TriggerHandler::CompleteAuctions,
            ));
            spec.requires_accounting_window = false;
            spec.coalesces_backlog = true;
            spec.depends_on = &[TriggerId::AdvanceAuctions];
            spec
        },
        // Reads finalized oracle VWAP history to call credis
        // positions. It has no dependency on the parent block's settlement
        // accounting.
        // The sweep takes its day from the block clock, so a missed slot
        // would only schedule the same day again.
        {
            let mut spec = const_spec!(TriggerSpec::new(
                TriggerId::ScheduleCredisCallPass.as_u32(),
                "schedule_credis_call_pass",
                (CALL_SWEEP_PERIOD_SECONDS, 0),
                202_000,
                TriggerHandler::ScheduleCredisCallPass,
            ));
            spec.requires_accounting_window = false;
            spec.coalesces_backlog = true;
            spec
        },
        // Schedules the latest completed UTC day for the call sweep.
        // Missed slots would schedule the same day against the current clock.
        {
            let mut spec = const_spec!(TriggerSpec::new(
                TriggerId::ScheduleNodCallPass.as_u32(),
                "schedule_nod_call_pass",
                (CALL_SWEEP_PERIOD_SECONDS, 0),
                202_000,
                TriggerHandler::ScheduleNodCallPass,
            ));
            spec.requires_accounting_window = false;
            spec.coalesces_backlog = true;
            spec
        },
    ])
}

pub const ACTIVE_TRIGGER_ARRAY: [TriggerSpec; 7] = match active_triggers() {
    Ok(specs) => specs,
    Err(_) => panic!("trigger periods and calibrated work budgets must be nonzero"),
};
pub const ACTIVE_TRIGGERS: &[TriggerSpec] = &ACTIVE_TRIGGER_ARRAY;

/// Returns the next slot strictly greater than `last_executed_at`.
/// A zero period returns [`CycleError::ZeroTriggerPeriod`].
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
        return Err(CycleError::ZeroTriggerPeriod);
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
/// the first due slot collapses a backlog for a poll-style trigger. The next
/// call then sees a slot in the future and stops re-firing.
/// A zero period returns [`CycleError::ZeroTriggerPeriod`].
/// A positive period cannot overflow `u64` when `block_ts >= offset`.
pub const fn last_fire_at(
    period: u64,
    offset: u64,
    block_ts: u64,
) -> core::result::Result<u64, CycleError> {
    if period == 0 {
        return Err(CycleError::ZeroTriggerPeriod);
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
    fn calibrated_budgets_are_nonzero_and_follow_checked_headroom_rule() {
        let maxima = [
            381_500u64, 50_300, 991_492, 50_300, 2_583_089, 50_300, 50_300,
        ];
        let triggers = active_triggers().expect("calibrated trigger table");
        let mut total = 0u64;
        for (spec, measured) in triggers.iter().zip(maxima) {
            let rounded = measured
                .checked_mul(4)
                .and_then(|gas| gas.checked_add(999))
                .map(|gas| gas / 1_000)
                .and_then(|units| units.checked_mul(1_000))
                .expect("measured handler headroom fits u64");
            assert!(spec.work_budget_gas() > 0);
            if spec.id != TriggerId::AdvanceAuctions.as_u32() {
                assert_eq!(spec.work_budget_gas(), rounded, "trigger {}", spec.id);
            }
            total = total
                .checked_add(spec.work_budget_gas())
                .expect("combined allowances fit u64");
        }
        assert_eq!(total, 22_093_000);
    }
}
