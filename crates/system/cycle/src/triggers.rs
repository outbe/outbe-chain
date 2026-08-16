//! Code-defined trigger registry.
//!
//! Triggers are declared as `const` data — there is no on-chain
//! registration path. Adding, removing, or re-parameterizing a trigger
//! is a hard-fork-coordinated code change. The dispatcher in
//! [`crate::runtime`] iterates this slice on every block and fires any
//! trigger whose next slot has been reached.

use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

/// Stable on-chain identifier for each trigger. The numeric values
/// must remain byte-equal forever — they are emitted as the indexed
/// `id` in [`crate::ICycle::CycleTriggerExecuted`] and persisted in
/// the [`crate::schema::Cycle`] mappings. New triggers append; never
/// renumber existing ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TriggerId {
    EmissionLimit1 = 0,
    IntexCallDaily = 1,
    WwdAdvanceNoon = 2,
    AuctionAdvance = 3,
    GemCallDaily = 4,
    AuctionClearing = 5,
    IntexQualifyNotify = 6,
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
    pub period_seconds: u64,
    /// Phase offset from unix epoch zero, in `[0, period_seconds)`.
    /// `period_seconds = 86_400, start_offset_seconds = 0` => UTC
    /// midnight slots; `period_seconds = 3600, start_offset_seconds =
    /// 1800` => "every hour at :30".
    pub start_offset_seconds: u64,
    /// when `true`, the dispatcher must additionally verify
    /// that V2 Phase 1 (`CertifiedParentAccounting`) has committed
    /// progress for the parent block — i.e.,
    /// `last_accounted_block_number >= window.end_inclusive` — before
    /// firing the handler. When `false` (e.g., a job that does NOT depend
    /// on parent accounting state) the handler runs on its own schedule
    /// without consulting [`outbe_primitives::accounting_progress::AccountingProgressView`].
    /// Default for the canonical emission-limit trigger is `true`.
    pub requires_accounting_window: bool,
    /// When `true` a backlog collapses to the latest due slot instead of
    /// replaying every missed one. Poll-style triggers want this; a handler
    /// that settles one specific slot (emission, advancement) must not.
    pub coalesces_backlog: bool,
    /// Handler invoked when a slot fires. Failure rolls back the
    /// trigger's checkpoint and leaves `last_executed_at` unchanged.
    pub handler: TriggerHandler,
}

#[derive(Clone, Copy)]
pub enum TriggerHandler {
    EmissionLimitDaily,
    IntexDaily,
    WwdAdvanceNoon,
    AuctionAdvance,
    GemCallDaily,
    AuctionClearing,
    IntexQualifyNotify,
}

impl TriggerHandler {
    /// Maximum number of sequential Metadosis command leases this handler can
    /// consume in one authenticated Cycle system transaction.
    pub const fn metadosis_mutation_lease_budget(self) -> u8 {
        match self {
            // Terminal allocation and the subsequent WWD process command.
            Self::EmissionLimitDaily => 2,
            Self::WwdAdvanceNoon => 1,
            Self::IntexDaily
            | Self::AuctionAdvance
            | Self::GemCallDaily
            | Self::AuctionClearing
            | Self::IntexQualifyNotify => 0,
        }
    }

    pub(crate) fn run(
        self,
        ctx: &BlockRuntimeContext,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
    ) -> Result<()> {
        match self {
            Self::EmissionLimitDaily => {
                crate::handler::run_emission_limit_daily(ctx, scope, parent)
            }
            Self::IntexDaily => outbe_intexfactory::called::run_daily(ctx),
            Self::WwdAdvanceNoon => {
                outbe_metadosis::commands::advance_active_worldwide_days(ctx, scope)
            }
            Self::AuctionAdvance => outbe_desis::tick_schedule(ctx),
            Self::GemCallDaily => outbe_gem::hooks::run_call_daily(ctx),
            Self::AuctionClearing => outbe_desis::tick_gate(ctx),
            Self::IntexQualifyNotify => {
                outbe_intexfactory::qualified::drain_qualify_notices(ctx)
            }
        }
    }
}

/// Exact bounded Metadosis command budget for one `CycleTick`. A long-gap block
/// can fire both the daily emission and noon advancement handlers, so their
/// budgets must be summed rather than taking only the common daily path.
#[must_use]
pub fn metadosis_mutation_lease_budget_per_tick() -> u8 {
    ACTIVE_TRIGGERS.iter().fold(0_u8, |budget, trigger| {
        budget.saturating_add(trigger.handler.metadosis_mutation_lease_budget())
    })
}

/// An e2e run walks a whole auction inside one day, so its stages need a cadence
/// that fits there rather than the production half-day one.
#[cfg(not(feature = "e2e-test"))]
const AUCTION_ADVANCE_PERIOD_SECONDS: u64 = 43_200;
#[cfg(feature = "e2e-test")]
const AUCTION_ADVANCE_PERIOD_SECONDS: u64 = 60;

/// Active trigger table. Order is informational only — the dispatcher
/// fires triggers independently per slot.
pub const fn active_triggers(metadosis_advance_interval_seconds: u64) -> [TriggerSpec; 7] {
    let production_default = outbe_chain_constants::DEFAULT_METADOSIS_ADVANCE_INTERVAL_SECONDS;
    let (wwd_period_seconds, wwd_start_offset_seconds) =
        if metadosis_advance_interval_seconds == production_default {
            // Preserve the historical noon-only dedicated trigger. The
            // midnight daily handler is the other production advancement.
            (86_400, production_default)
        } else {
            // Short genesis profiles drive the dedicated advancement directly.
            (metadosis_advance_interval_seconds, 0)
        };
    [
        TriggerSpec {
            id: TriggerId::EmissionLimit1.as_u32(),
            label: "emission_limit_1",
            period_seconds: 86_400,
            start_offset_seconds: 0,
            // daily emission orchestrator settles the previous UTC
            // day; it MUST observe the parent block's Phase 1 accounting before
            // firing, otherwise validator-pool top-ups and daily-fee reads would
            // race the parent-finalization tx.
            requires_accounting_window: true,
            coalesces_backlog: false,
            handler: TriggerHandler::EmissionLimitDaily,
        },
        TriggerSpec {
            id: TriggerId::IntexCallDaily.as_u32(),
            label: "intex_call_daily",
            period_seconds: 86_400,
            start_offset_seconds: 0,
            // Reads finalized oracle VWAP history and marks series Called; no
            // dependency on the parent block's settlement accounting.
            requires_accounting_window: false,
            coalesces_backlog: false,
            handler: TriggerHandler::IntexDaily,
        },
        TriggerSpec {
            id: TriggerId::WwdAdvanceNoon.as_u32(),
            label: "wwd_advance",
            // Defaults to every 12 hours (midnight/noon). LocalNet can use a
            // shorter genesis-bound interval without changing daily creation.
            period_seconds: wwd_period_seconds,
            start_offset_seconds: wwd_start_offset_seconds,
            // Pure status-window walk over active WorldwideDays: reads
            // Metadosis windows and the Oracle, never the parent block's
            // settlement accounting. Day creation and READY settlement stay
            // on the midnight `emission_limit_1` trigger.
            requires_accounting_window: false,
            coalesces_backlog: false,
            handler: TriggerHandler::WwdAdvanceNoon,
        },
        TriggerSpec {
            id: TriggerId::AuctionAdvance.as_u32(),
            label: "auction_advance",
            period_seconds: AUCTION_ADVANCE_PERIOD_SECONDS,
            start_offset_seconds: 0,
            // Gated like emission_limit_1 so the brief it writes and this start
            // land in the same slot.
            requires_accounting_window: true,
            coalesces_backlog: false,
            handler: TriggerHandler::AuctionAdvance,
        },
        TriggerSpec {
            id: TriggerId::AuctionClearing.as_u32(),
            label: "auction_clearing",
            // Polls the fan-in gate `auction_advance` arms, so it runs far more
            // often than the stage schedule advances.
            period_seconds: 600,
            start_offset_seconds: 0,
            // Clears from bids already ingested and the router's frozen target
            // list; no dependency on the parent block's settlement accounting.
            requires_accounting_window: false,
            // A poll has nothing to replay: a gap collapses to one clearing sweep.
            coalesces_backlog: true,
            handler: TriggerHandler::AuctionClearing,
        },
        TriggerSpec {
            id: TriggerId::GemCallDaily.as_u32(),
            label: "gem_call_daily",
            period_seconds: 86_400,
            start_offset_seconds: 0,
            // Reads finalized oracle VWAP history to force-call / forfeit-burn gems;
            // no dependency on the parent block's settlement accounting.
            requires_accounting_window: false,
            coalesces_backlog: false,
            handler: TriggerHandler::GemCallDaily,
        },
        TriggerSpec {
            id: TriggerId::IntexQualifyNotify.as_u32(),
            label: "intex_qualify_notify",
            period_seconds: 600,
            start_offset_seconds: 0,
            // Drains a queue the qualify sweep filled; reads no accounting state.
            requires_accounting_window: false,
            // A poll has nothing to replay: a gap collapses to one drain.
            coalesces_backlog: true,
            handler: TriggerHandler::IntexQualifyNotify,
        },
    ]
}

pub const ACTIVE_TRIGGER_ARRAY: [TriggerSpec; 7] =
    active_triggers(outbe_chain_constants::DEFAULT_METADOSIS_ADVANCE_INTERVAL_SECONDS);
pub const ACTIVE_TRIGGERS: &[TriggerSpec] = &ACTIVE_TRIGGER_ARRAY;

/// Returns the next slot strictly greater than `last_executed_at`.
/// Pure function: same `(period, offset, last)` tuple always returns
/// the same slot. Monotonically non-decreasing in `last_executed_at`
/// (and strictly increasing for `last_executed_at` past the first
/// slot).
///
/// Invariants:
/// * result `>= offset`,
/// * `(result - offset) % period == 0`,
/// * `result > last_executed_at`.
pub fn next_fire_at(period: u64, offset: u64, last_executed_at: u64) -> u64 {
    if last_executed_at < offset {
        return offset;
    }
    let diff = last_executed_at - offset;
    let k = diff / period + 1;
    offset + k * period
}

/// Returns the latest slot at or before `block_ts`. Recording this instead of
/// the first due slot is what collapses a backlog for a poll-style trigger:
/// the next call then sees a slot in the future and stops re-firing.
pub fn last_fire_at(period: u64, offset: u64, block_ts: u64) -> u64 {
    if block_ts < offset {
        return offset;
    }
    offset + (block_ts - offset) / period * period
}

#[cfg(test)]
mod protocol_parameter_tests {
    use super::*;

    #[test]
    fn only_wwd_advancement_uses_the_genesis_interval() {
        let configured = active_triggers(10);
        assert_eq!(configured[0].period_seconds, 86_400);
        assert_eq!(configured[1].period_seconds, 86_400);
        assert_eq!(configured[2].period_seconds, 10);
        assert_eq!(configured[2].start_offset_seconds, 0);
        assert_eq!(configured[3].period_seconds, 43_200);
        assert_eq!(configured[4].period_seconds, 600);
        assert!(matches!(
            configured[4].handler,
            TriggerHandler::AuctionClearing
        ));
        assert_eq!(configured[5].period_seconds, 86_400);
        assert!(matches!(
            configured[5].handler,
            TriggerHandler::GemCallDaily
        ));

        let defaults =
            active_triggers(outbe_chain_constants::DEFAULT_METADOSIS_ADVANCE_INTERVAL_SECONDS);
        assert_eq!(defaults[2].period_seconds, 86_400);
        assert_eq!(defaults[2].start_offset_seconds, 43_200);
    }
}
