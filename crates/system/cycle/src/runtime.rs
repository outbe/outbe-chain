//! Per-block trigger dispatch loop.
//!
//! Runs from [`crate::lifecycle::CycleLifecycle::begin_block`] on every
//! block during the begin-zone CycleTick phase. Iterates the genesis-resolved
//! trigger table
//! and fires any trigger whose next slot has been reached.

use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
};

use crate::handler::{protocol_day_action, ProtocolDayAction};
use crate::schema::Cycle;
use crate::state::{accounting_gate_blocks, EvmAccountingProgress};
use crate::triggers::{active_triggers, last_fire_at, next_fire_at, TriggerId, TriggerSpec};
use crate::ICycle;

/// Dispatches every active trigger whose `next_fire_at` is `<=
/// ctx.block.timestamp`. The checkpoint around a handler contains only that
/// handler. A revert rolls the handler back, records `recorded_at`, emits
/// `CycleTriggerFailed`, and the walk continues. Any other handler error
/// returns to the caller, and CycleTick rejects the block. A recorded slot
/// stays consumed until its next scheduled window.
///
/// The typical case is a slow-running chain that produces blocks every few
/// seconds. In that case the dispatcher is a near-noop on every block. It
/// fires ProtocolCycle only on the first block whose timestamp crosses
/// the next UTC-hour boundary.
pub fn dispatch_triggers(ctx: &BlockRuntimeContext) -> Result<()> {
    let triggers =
        active_triggers(outbe_chain_constants::get_metadosis_advance_interval_seconds())?;
    // Active ids are 0, 2, 4, and 8. Retired ids must not be reused.
    let mut failed = [false; 9];

    for spec in &triggers {
        if let Some(slot) = due_slot(ctx, spec)? {
            dispatch_due(ctx, spec, slot, &mut failed)?;
        }
    }

    Ok(())
}

/// Runs one due trigger. A dependency failure consumes the slot and does not
/// call the handler. A handler revert consumes the slot and emits
/// `CycleTriggerFailed`. Any other handler error returns immediately.
fn dispatch_due(
    ctx: &BlockRuntimeContext,
    spec: &TriggerSpec,
    slot: DueSlot,
    failed: &mut [bool; 9],
) -> Result<()> {
    if spec
        .depends_on
        .iter()
        .any(|id| failed[id.as_u32() as usize])
    {
        record_slot(ctx, spec, slot.recorded_at)?;
        failed[spec.id as usize] = true;
        tracing::info!(
            target: "outbe::cycle",
            trigger_id = spec.id,
            label = spec.label,
            recorded_at = slot.recorded_at,
            block_ts = ctx.block.timestamp,
            block_number = ctx.block.block_number,
            "cycle trigger skipped because a dependency failed in this walk"
        );
        return Ok(());
    }

    match ctx.storage.with_checkpoint(|| spec.handler.run(ctx)) {
        Ok(()) => record_executed(ctx, spec, slot),
        Err(err @ (PrecompileError::Revert(_) | PrecompileError::RevertBytes(_))) => {
            record_failed(ctx, spec, slot, &err)?;
            failed[spec.id as usize] = true;
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// Writes the consumed slot outside the handler checkpoint.
fn record_slot(ctx: &BlockRuntimeContext, spec: &TriggerSpec, recorded_at: u64) -> Result<()> {
    let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
    cycle.last_executed_at.write(&spec.id, recorded_at)?;
    cycle
        .last_executed_block_number
        .write(&spec.id, ctx.block.block_number)?;
    Ok(())
}

/// Records a successful slot and emits `CycleTriggerExecuted`.
fn record_executed(ctx: &BlockRuntimeContext, spec: &TriggerSpec, slot: DueSlot) -> Result<()> {
    let block_ts = ctx.block.timestamp;
    let block_number = ctx.block.block_number;
    record_slot(ctx, spec, slot.recorded_at)?;
    let mut cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
    cycle.emit(ICycle::CycleTriggerExecuted {
        id: spec.id,
        scheduledAt: slot.recorded_at,
        blockTimestamp: block_ts,
        blockNumber: block_number,
    })?;
    tracing::info!(
        target: "outbe::cycle",
        trigger_id = spec.id,
        label = spec.label,
        scheduled_at = slot.scheduled_at,
        recorded_at = slot.recorded_at,
        block_ts,
        block_number,
        "cycle trigger fired"
    );
    Ok(())
}

/// Records a reverted slot and emits `CycleTriggerFailed` with no error text.
fn record_failed(
    ctx: &BlockRuntimeContext,
    spec: &TriggerSpec,
    slot: DueSlot,
    err: &PrecompileError,
) -> Result<()> {
    let block_ts = ctx.block.timestamp;
    let block_number = ctx.block.block_number;
    record_slot(ctx, spec, slot.recorded_at)?;
    let mut cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
    cycle.emit(ICycle::CycleTriggerFailed {
        id: spec.id,
        scheduledAt: slot.recorded_at,
        blockTimestamp: block_ts,
        blockNumber: block_number,
    })?;
    tracing::error!(
        target: "outbe::cycle",
        trigger_id = spec.id,
        label = spec.label,
        scheduled_at = slot.scheduled_at,
        recorded_at = slot.recorded_at,
        block_ts,
        block_number,
        error = ?err,
        "cycle trigger handler reverted; checkpoint rolled back and the slot was consumed"
    );
    Ok(())
}

/// Slot that one trigger fires in the current block.
#[derive(Clone, Copy, Debug)]
struct DueSlot {
    /// Next slot after `last_executed_at`.
    scheduled_at: u64,
    /// Slot that the dispatcher records as `last_executed_at`.
    recorded_at: u64,
}

/// Returns the slot that `spec` fires in the current block, or `None` when
/// the trigger does not fire.
fn due_slot(ctx: &BlockRuntimeContext, spec: &TriggerSpec) -> Result<Option<DueSlot>> {
    let block_ts = ctx.block.timestamp;
    let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
    let last_executed_at = cycle.last_executed_at.read(&spec.id)?;

    // First-ever encounter for this trigger: anchor `last_executed_at`
    // at the current block timestamp. The first real fire is then the
    // next slot strictly after this point. Without this anchor,
    // every chain would fire on its first block because real genesis
    // timestamps are far beyond the first unix-epoch schedule slot.
    if last_executed_at == 0 {
        anchor_first_encounter(ctx, spec)?;
        return Ok(None);
    }

    let scheduled_at = next_fire_at(
        spec.period_seconds(),
        spec.start_offset_seconds,
        last_executed_at,
    )?;
    if block_ts < scheduled_at || slot_deferred(ctx, spec, &cycle)? {
        return Ok(None);
    }

    // A poll records the latest due slot, so a gap costs one firing rather
    // than one per missed slot.
    let recorded_at = if spec.coalesces_backlog {
        last_fire_at(spec.period_seconds(), spec.start_offset_seconds, block_ts)?
    } else {
        scheduled_at
    };
    Ok(Some(DueSlot {
        scheduled_at,
        recorded_at,
    }))
}

/// Writes the current block timestamp as the first `last_executed_at` of `spec`.
fn anchor_first_encounter(ctx: &BlockRuntimeContext, spec: &TriggerSpec) -> Result<()> {
    let block_ts = ctx.block.timestamp;
    let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
    cycle.last_executed_at.write(&spec.id, block_ts)?;
    tracing::debug!(
        target: "outbe::cycle",
        trigger_id = spec.id,
        label = spec.label,
        block_ts,
        "cycle trigger anchored on first encounter; first fire deferred to next slot"
    );
    Ok(())
}

/// Returns `true` when a due slot of `spec` must stay pending until a later
/// block. The deferral changes no state.
fn slot_deferred(ctx: &BlockRuntimeContext, spec: &TriggerSpec, cycle: &Cycle<'_>) -> Result<bool> {
    // refuse to fire a gated trigger until Phase 1
    // has accounted the parent block. Under the V2 reorder,
    // Phase 1 commits BEFORE Phase 2 (`CycleTick`). Thus
    // this gate is normally vacuously satisfied. It fires only when
    // a regression reorders the phases or a new trigger reads state
    // that races the parent-finalization tx. Defer silently (no
    // error, no state change), so the trigger retries on the next
    // block.
    let progress = EvmAccountingProgress::new(ctx);
    if accounting_gate_blocks(spec, &progress, &ctx.block)? {
        tracing::debug!(
            target: "outbe::cycle",
            trigger_id = spec.id,
            label = spec.label,
            block_number = ctx.block.block_number,
            "cycle trigger deferred: Phase 1 has not yet accounted the parent block"
        );
        return Ok(true);
    }
    protocol_cycle_awaits_late_window(ctx, spec, cycle)
}

/// At midnight, keep the protocol slot pending until the previous day's final
/// late-vote window has executed. Other triggers retain their own cadence.
/// This gate uses no sleep or wall-clock timer.
fn protocol_cycle_awaits_late_window(
    ctx: &BlockRuntimeContext,
    spec: &TriggerSpec,
    cycle: &Cycle<'_>,
) -> Result<bool> {
    if spec.id != TriggerId::ProtocolCycle.as_u32() {
        return Ok(false);
    }
    let day_action = protocol_day_action(
        cycle.active_utc_day.read()?,
        outbe_primitives::time::timestamp_to_date_key(ctx.block.timestamp),
    )?;
    match day_action {
        ProtocolDayAction::SettlePrevious { day } => {
            Ok(!outbe_rewards::api::day_participation_complete(ctx, day)?)
        }
        ProtocolDayAction::SameDay | ProtocolDayAction::SkipMissed { .. } => Ok(false),
    }
}
