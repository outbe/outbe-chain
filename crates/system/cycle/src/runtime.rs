//! Per-block trigger dispatch loop.
//!
//! Runs from [`crate::lifecycle::CycleLifecycle::begin_block`] on every
//! block during the begin-zone CycleTick phase. Iterates [`crate::triggers::ACTIVE_TRIGGERS`]
//! and fires any trigger whose next slot has been reached.

use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::schema::Cycle;
use crate::state::{accounting_gate_blocks, EvmAccountingProgress};
use crate::triggers::{next_fire_at, ACTIVE_TRIGGERS};
use crate::ICycle;

/// Dispatches every active trigger whose `next_fire_at` is `<=
/// ctx.block.timestamp`. Each fired trigger is wrapped in its own
/// storage checkpoint, so a handler failure rolls back its writes and
/// leaves `last_executed_at` unchanged for retry on the next block.
///
/// For the typical case of a slow-running chain that produces blocks
/// every few seconds, the dispatcher is a near-noop on every block and
/// fires the daily trigger only on the first block whose timestamp
/// crosses UTC midnight.
pub fn dispatch_triggers(ctx: &BlockRuntimeContext) -> Result<()> {
    let block_ts = ctx.block.timestamp;
    let block_number = ctx.block.block_number;

    for spec in ACTIVE_TRIGGERS {
        let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
        let last_executed_at = cycle.last_executed_at.read(&spec.id)?;

        // First-ever encounter for this trigger: anchor `last_executed_at`
        // at the current block timestamp so the first real fire is the
        // next slot strictly after this point. Without this anchor,
        // every chain would fire on its first block (because `block_ts`
        // on a real chain is always `>> 86_400` so
        // `next_fire_at(_, 0, 0) = 86_400` is always reached).
        if last_executed_at == 0 {
            cycle.last_executed_at.write(&spec.id, block_ts)?;
            tracing::debug!(
                target: "outbe::cycle",
                trigger_id = spec.id,
                label = spec.label,
                block_ts,
                "cycle trigger anchored on first encounter; first fire deferred to next slot"
            );
            continue;
        }

        let scheduled_at = next_fire_at(
            spec.period_seconds,
            spec.start_offset_seconds,
            last_executed_at,
        );
        if block_ts < scheduled_at {
            continue;
        }

        // refuse to fire a gated trigger until Phase 1
        // has accounted the parent block. Under the V2 reorder
        //, Phase 1 commits BEFORE Phase 2 (`CycleTick`), so
        // this gate is normally vacuously satisfied; it fires only when
        // a regression reorders the phases or a new trigger reads state
        // that races the parent-finalization tx. Defer silently — no
        // error, no state change — so the trigger retries on the next
        // block.
        let progress = EvmAccountingProgress::new(ctx);
        if accounting_gate_blocks(spec, &progress, &ctx.block)? {
            tracing::debug!(
                target: "outbe::cycle",
                trigger_id = spec.id,
                label = spec.label,
                block_number,
                "cycle trigger deferred: Phase 1 has not yet accounted the parent block"
            );
            continue;
        }

        let result = ctx.storage.with_checkpoint(|| {
            (spec.handler)(ctx)?;
            let mut cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
            cycle.last_executed_at.write(&spec.id, scheduled_at)?;
            cycle
                .last_executed_block_number
                .write(&spec.id, block_number)?;
            cycle.emit(ICycle::CycleTriggerExecuted {
                id: spec.id,
                scheduledAt: scheduled_at,
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
                return Err(err);
            }
        }
    }

    Ok(())
}
