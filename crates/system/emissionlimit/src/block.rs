//! Terminal-Metadosis dispatch helper.
//!
//! After (Phase 4 of the Cycle epic), EmissionLimit is no longer wired into the per-block
//! lifecycle. The previous `EmissionLimitLifecycle::begin_block` / `run_begin_block` /
//! `dispatch_block_emission` triple was removed. The day orchestration runs in the new
//! Cycle module. That module:
//!
//! - reads the closed-form `day_emission_limit`
//! - calls [`crate::allocation::allocate_emission`] with the 5-sink active table
//! - hands the reward pools to `outbe_agentreward::distribute_daily`
//! - forwards the Metadosis terminal portion through [`dispatch_terminal_remainder_at`] below.
//!
//! This file is intentionally tiny. It owns the purpose-bound terminal dispatch calls that
//! Cycle and late fee settlement use. Keeping them distinct prevents a non-daily residue from
//! becoming a daily base-limit producer.

use alloy_primitives::U256;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::errors::EmissionLimitError;

/// Sends the daily Cycle terminal allocation to the Metadosis sink, anchored at
/// `timestamp`.
///
/// The Cycle day handler dispatches the previous UTC day's terminal Metadosis
/// amount. The sink must use that previous-day timestamp so that worldwide-day
/// accounting lands in the right bucket. This holds regardless of when the call
/// physically runs.
///
/// Returns `Revert` if the Metadosis sink reports any unused amount.
/// The terminal sink must consume the full amount.
pub fn dispatch_terminal_remainder_at(
    ctx: &BlockRuntimeContext,
    amount: U256,
    timestamp: u64,
) -> Result<()> {
    let mut terminal_block = ctx.block.clone();
    terminal_block.timestamp = timestamp;
    let terminal_ctx = BlockRuntimeContext::new(terminal_block, ctx.storage.clone());
    let unused = outbe_metadosis::commands::apply_cycle_day_limit(&terminal_ctx, amount)?;
    if !unused.is_zero() {
        return Err(EmissionLimitError::TerminalSinkReturnedUnusedAmount.into());
    }
    Ok(())
}

/// Recycles a late fee-settlement residue into terminal Metadosis headroom.
///
/// This function is deliberately distinct from [`dispatch_terminal_remainder_at`].
/// Only the daily Cycle allocation forms a base day limit. Late residues
/// accumulate in the Promis Limit for the next unformed limit.
pub fn dispatch_late_settlement_residue_at(
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
    let unused = outbe_metadosis::commands::apply_late_settlement_headroom(&terminal_ctx, amount)?;
    if !unused.is_zero() {
        return Err(EmissionLimitError::LateSettlementSinkReturnedUnusedAmount.into());
    }
    Ok(())
}
