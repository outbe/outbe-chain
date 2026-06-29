//! Terminal-Metadosis dispatch helper.
//!
//! After (Phase 4 of the Cycle epic) EmissionLimit is no
//! longer wired into the per-block lifecycle. The previous
//! `EmissionLimitLifecycle::begin_block` / `run_begin_block` /
//! `dispatch_block_emission` triple has been removed; the day
//! orchestration runs out of the new Cycle module which
//! reads the closed-form `day_emission_limit`, calls
//! [`crate::allocation::allocate_emission`] with the 6-sink active
//! table, hands non-validator pools to `outbe_agentreward::distribute_daily`,
//! and forwards the Metadosis terminal portion through
//! [`dispatch_terminal_remainder_at`] below.
//!
//! This file is intentionally tiny — it only owns the terminal
//! dispatch call so that the Cycle handler can route the Metadosis
//! residue to a deterministic timestamp (the finalized block's UTC
//! day, not the dispatching block's) without going through a full
//! sink table roundtrip.

use alloy_primitives::U256;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
};

/// Sends emission returned after delayed settlement to the Metadosis
/// terminal sink, anchored at `timestamp`.
///
/// Delayed settlement happens when finalized metadata is executed —
/// which may be later than the finalized block itself — and when the
/// Cycle day handler dispatches the previous UTC day's terminal
/// Metadosis amount. The terminal sink must use the finalized /
/// previous-day timestamp so Metadosis worldwide-day accounting lands
/// in the right bucket regardless of when the call physically runs.
///
/// Returns `Fatal` if the Metadosis sink reports any unused amount —
/// the terminal sink is required to be a sink, not a pass-through.
pub fn dispatch_terminal_remainder_at(
    ctx: &BlockRuntimeContext,
    amount: U256,
    timestamp: u64,
) -> Result<()> {
    if amount.is_zero() {
        return Ok(());
    }

    let mut terminal_block = ctx.block.clone();
    terminal_block.timestamp = timestamp;
    let terminal_ctx = BlockRuntimeContext::new(terminal_block, ctx.storage.clone());
    let unused = outbe_metadosis::daily_accumulation::apply(&terminal_ctx, amount)?;
    if !unused.is_zero() {
        return Err(PrecompileError::Revert(
            "terminal emission sink returned unused amount".into(),
        ));
    }
    Ok(())
}
