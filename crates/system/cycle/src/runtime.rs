//! Per-block trigger dispatch loop.
//!
//! Runs from [`crate::lifecycle::CycleLifecycle::begin_block`] on every
//! block during the begin-zone CycleTick phase. Iterates the genesis-resolved
//! trigger table
//! and fires any trigger whose next slot has been reached.

use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::handler::{protocol_day_action, ProtocolDayAction};
use crate::schema::Cycle;
use crate::state::{accounting_gate_blocks, EvmAccountingProgress};
use crate::triggers::{active_triggers, last_fire_at, next_fire_at, TriggerId, TriggerSpec};
use crate::ICycle;

/// Dispatches every active trigger whose `next_fire_at` is `<=
/// ctx.block.timestamp`. The dispatcher wraps each fired trigger in its own
/// storage checkpoint. A handler failure rolls those writes back and returns
/// the error. CycleTick rejects the block on that error.
/// The same slot does not wait for the next block.
///
/// The typical case is a slow-running chain that produces blocks every few
/// seconds. In that case the dispatcher is a near-noop on every block. It
/// fires ProtocolCycle only on the first block whose timestamp crosses
/// the next UTC-hour boundary.
pub fn dispatch_triggers(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<()> {
    let triggers = active_triggers(outbe_chain_constants::get_metadosis_advance_interval_seconds());

    for spec in &triggers {
        if let Some(slot) = due_slot(ctx, spec)? {
            fire_trigger(ctx, scope, parent, spec, slot)?;
        }
    }

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
        spec.period_seconds,
        spec.start_offset_seconds,
        last_executed_at,
    );
    if block_ts < scheduled_at || slot_deferred(ctx, spec, &cycle)? {
        return Ok(None);
    }

    // A poll records the latest due slot, so a gap costs one firing rather
    // than one per missed slot.
    let recorded_at = if spec.coalesces_backlog {
        last_fire_at(spec.period_seconds, spec.start_offset_seconds, block_ts)
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

/// Runs the handler of `spec` for `slot` in its own storage checkpoint, then
/// records the slot and emits `CycleTriggerExecuted`. A failure rolls the
/// checkpoint back and returns the error.
fn fire_trigger(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    spec: &TriggerSpec,
    slot: DueSlot,
) -> Result<()> {
    let block_ts = ctx.block.timestamp;
    let block_number = ctx.block.block_number;
    let DueSlot {
        scheduled_at,
        recorded_at,
    } = slot;

    let result = ctx.storage.with_checkpoint(|| {
        spec.handler.run(ctx, scope, parent)?;
        let mut cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
        cycle.last_executed_at.write(&spec.id, recorded_at)?;
        cycle
            .last_executed_block_number
            .write(&spec.id, block_number)?;
        cycle.emit(ICycle::CycleTriggerExecuted {
            id: spec.id,
            scheduledAt: recorded_at,
            blockTimestamp: block_ts,
            blockNumber: block_number,
        })?;
        Ok::<(), outbe_primitives::error::PrecompileError>(())
    });

    match result {
        Ok(()) => {
            tracing::info!(
                target: "outbe::cycle",
                trigger_id = spec.id,
                label = spec.label,
                scheduled_at,
                block_ts,
                block_number,
                "cycle trigger fired"
            );
            Ok(())
        }
        Err(err) => {
            tracing::error!(
                target: "outbe::cycle",
                trigger_id = spec.id,
                label = spec.label,
                scheduled_at,
                block_ts,
                block_number,
                error = ?err,
                "cycle trigger handler failed; checkpoint rolled back, will retry next block"
            );
            Err(err)
        }
    }
}
