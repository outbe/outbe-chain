//! Code-defined trigger registry.
//!
//! Triggers are declared as `const` data — there is no on-chain
//! registration path. Adding, removing, or re-parameterizing a trigger
//! is a hard-fork-coordinated code change. The dispatcher in
//! [`crate::runtime`] iterates this slice on every block and fires any
//! trigger whose next slot has been reached.

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
}

impl TriggerId {
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

/// One trigger's static configuration plus its handler function
/// pointer. Handler signature is `fn(&BlockRuntimeContext) -> Result<()>`.
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
    /// Handler invoked when a slot fires. Failure rolls back the
    /// trigger's checkpoint and leaves `last_executed_at` unchanged.
    pub handler: fn(&BlockRuntimeContext) -> Result<()>,
}

/// Active trigger table. Order is informational only — the dispatcher
/// fires triggers independently per slot.
pub const ACTIVE_TRIGGERS: &[TriggerSpec] = &[
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
        handler: crate::handler::run_emission_limit_daily,
    },
    TriggerSpec {
        id: TriggerId::IntexCallDaily.as_u32(),
        label: "intex_call_daily",
        period_seconds: 86_400,
        start_offset_seconds: 0,
        // Reads finalized oracle VWAP history and marks series Called; no
        // dependency on the parent block's settlement accounting.
        requires_accounting_window: false,
        handler: outbe_intexfactory::called::run_daily,
    },
    TriggerSpec {
        id: TriggerId::WwdAdvanceNoon.as_u32(),
        label: "wwd_advance_noon",
        period_seconds: 86_400,
        // WWD forming/offering window edges land at 12:00 UTC
        // (`forming_end = forming_start(10:00 UTC) + 50h`); with only the
        // midnight tick every 12:00 transition was applied ~12h late.
        start_offset_seconds: 43_200,
        // Pure status-window walk over active WorldwideDays: reads
        // Metadosis windows and the Oracle, never the parent block's
        // settlement accounting. Day creation and READY settlement stay
        // on the midnight `emission_limit_1` trigger.
        requires_accounting_window: false,
        handler: outbe_metadosis::runtime::advance_active_worldwide_days,
    },
];

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
