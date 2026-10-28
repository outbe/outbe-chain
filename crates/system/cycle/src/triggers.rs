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
/// Factory scheduler ids 1, 3, 5, and 6 are retired and must not be reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TriggerId {
    ProtocolCycle = 0,
    AuctionAdvance = 2,
    AuctionClearing = 4,
    IntexDrainParked = 8,
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
    /// [`outbe_primitives::accounting_progress::AccountingProgressView`].
    /// Default for the canonical protocol-cycle trigger is `true`.
    pub requires_accounting_window: bool,
    /// When `true` a backlog collapses to the latest due slot instead of
    /// replaying every missed one. ProtocolCycle owns its UTC-day decision,
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

#[derive(Clone, Copy)]
pub enum TriggerHandler {
    ProtocolCycle,
    AuctionAdvance,
    AuctionClearing,
    IntexDrainParked,
}

impl TriggerSpec {
    /// Builds a schedule. A zero period is the only rejected setting.
    /// `start_offset_seconds >= period_seconds` stays valid.
    /// The two behavior flags start false. `depends_on` starts empty.
    /// The trigger table and fixtures set them.
    pub const fn new(
        id: u32,
        label: &'static str,
        period_seconds: u64,
        start_offset_seconds: u64,
        handler: TriggerHandler,
    ) -> core::result::Result<Self, CycleError> {
        if period_seconds == 0 {
            return Err(CycleError::ZeroTriggerPeriod);
        }
        Ok(Self {
            id,
            label,
            period_seconds,
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
}

impl TriggerHandler {
    pub(crate) fn run(self, ctx: &BlockRuntimeContext) -> Result<()> {
        match self {
            Self::ProtocolCycle => crate::handler::run_protocol_cycle(ctx),
            Self::AuctionAdvance => outbe_desis::tick_schedule(ctx),
            Self::AuctionClearing => outbe_desis::tick_gate(ctx),
            Self::IntexDrainParked => outbe_intexfactory::parked::drain(ctx),
        }
    }
}

/// A brief only becomes a live auction on this tick: a slower cadence would spend
/// the commit window on waiting for the start.
///
/// An e2e day is minutes long, so it advances faster still.
#[cfg(not(feature = "e2e-test"))]
const AUCTION_ADVANCE_PERIOD_SECONDS: u64 = 3_600;
#[cfg(feature = "e2e-test")]
const AUCTION_ADVANCE_PERIOD_SECONDS: u64 = 60;

/// Cadence of the auction clearing poll, shortened for the same reason.
#[cfg(not(feature = "e2e-test"))]
const OUTBOUND_POLL_PERIOD_SECONDS: u64 = 600;
#[cfg(feature = "e2e-test")]
const OUTBOUND_POLL_PERIOD_SECONDS: u64 = 30;

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

/// Active trigger table in permanent numeric-id order.
/// The dispatcher walks this array in order inside one block.
/// A handler revert records the slot and continues the walk.
/// Any other handler error stops the walk.
/// A due trigger whose dependency failed in this walk records its slot
/// and does not run.
/// AuctionAdvance must stay after ProtocolCycle.
/// Same-slot brief and auction order depends on that sequence.
pub const fn active_triggers(
    metadosis_advance_interval_seconds: u64,
) -> core::result::Result<[TriggerSpec; 4], CycleError> {
    Ok([
        // ProtocolCycle settles a contiguous completed UTC day. It MUST
        // observe the parent block's Phase 1 accounting before it fires.
        // Otherwise validator-pool top-ups and daily-fee reads would
        // race the parent-finalization tx.
        // The handler owns the calendar decision, so missed hourly slots
        // collapse to one execution at the latest due boundary.
        {
            let mut spec = const_spec!(TriggerSpec::new(
                TriggerId::ProtocolCycle.as_u32(),
                "protocol_cycle",
                metadosis_advance_interval_seconds,
                0,
                TriggerHandler::ProtocolCycle,
            ));
            spec.requires_accounting_window = true;
            spec.coalesces_backlog = true;
            spec
        },
        // Gated like `protocol_cycle`, so the brief it writes and this start
        // land in the same slot.
        // A poll, not a calendar slot. The handler reads the block clock and
        // sweeps every scheduled day. So replaying a missed slot does nothing
        // that the next firing would not already do.
        {
            let mut spec = const_spec!(TriggerSpec::new(
                TriggerId::AuctionAdvance.as_u32(),
                "auction_advance",
                AUCTION_ADVANCE_PERIOD_SECONDS,
                0,
                TriggerHandler::AuctionAdvance,
            ));
            spec.requires_accounting_window = true;
            spec.coalesces_backlog = true;
            spec.depends_on = &[TriggerId::ProtocolCycle];
            spec
        },
        // Polls the fan-in gate `auction_advance` arms, so it runs far more
        // often than the stage schedule advances.
        // Clears from bids already ingested and the router's frozen target
        // list. It has no dependency on the parent block's settlement accounting.
        // A poll has nothing to replay: a gap collapses to one clearing sweep.
        {
            let mut spec = const_spec!(TriggerSpec::new(
                TriggerId::AuctionClearing.as_u32(),
                "auction_clearing",
                OUTBOUND_POLL_PERIOD_SECONDS,
                0,
                TriggerHandler::AuctionClearing,
            ));
            spec.requires_accounting_window = false;
            spec.coalesces_backlog = true;
            spec.depends_on = &[TriggerId::AuctionAdvance];
            spec
        },
        // Polls what the origin router parked, on the same cadence as the other outbound polls.
        // Reads the router's own queues. It has no dependency on the parent block's
        // accounting.
        // A poll has nothing to replay: a gap collapses to one sweep.
        {
            let mut spec = const_spec!(TriggerSpec::new(
                TriggerId::IntexDrainParked.as_u32(),
                "intex_drain_parked",
                OUTBOUND_POLL_PERIOD_SECONDS,
                0,
                TriggerHandler::IntexDrainParked,
            ));
            spec.requires_accounting_window = false;
            spec.coalesces_backlog = true;
            spec
        },
    ])
}

pub const ACTIVE_TRIGGER_ARRAY: [TriggerSpec; 4] =
    match active_triggers(outbe_chain_constants::DEFAULT_METADOSIS_ADVANCE_INTERVAL_SECONDS) {
        Ok(specs) => specs,
        Err(_) => panic!("default metadosis interval is nonzero"),
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
    fn protocol_cycle_uses_the_genesis_interval() {
        let configured = active_triggers(10).expect("interval 10");
        assert_eq!(configured[0].period_seconds(), 10);
        assert_eq!(configured[0].start_offset_seconds, 0);
        assert_eq!(
            configured[1].period_seconds(),
            AUCTION_ADVANCE_PERIOD_SECONDS
        );
        assert_eq!(configured[1].start_offset_seconds, 0);
        assert_eq!(configured[2].period_seconds(), OUTBOUND_POLL_PERIOD_SECONDS);
        assert!(matches!(
            configured[2].handler,
            TriggerHandler::AuctionClearing
        ));
        assert_eq!(configured[3].period_seconds(), OUTBOUND_POLL_PERIOD_SECONDS);
        assert_eq!(configured[3].id, TriggerId::IntexDrainParked.as_u32());
        assert!(matches!(
            configured[3].handler,
            TriggerHandler::IntexDrainParked
        ));

        let defaults =
            active_triggers(outbe_chain_constants::DEFAULT_METADOSIS_ADVANCE_INTERVAL_SECONDS)
                .expect("default interval");
        assert_eq!(defaults[0].period_seconds(), 3_600);
        assert_eq!(defaults[0].start_offset_seconds, 0);
    }
}
