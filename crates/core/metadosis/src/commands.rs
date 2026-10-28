use alloy_primitives::U256;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    storage::{
        metadosis_advance_due_binding, metadosis_init_genesis_binding,
        metadosis_process_ready_binding, MetadosisCycleLifecycle,
    },
};

use crate::commit::commit_transition;

pub fn init_genesis_day(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    let binding = metadosis_init_genesis_binding(
        ctx.block.chain_id,
        ctx.block.block_number,
        ctx.block.timestamp,
    );
    commit_transition::<MetadosisCycleLifecycle, _>(ctx.storage.clone(), binding, |_| {
        crate::runtime::init_genesis_day(ctx)
    })
}

pub fn start_metadosis(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    let binding = metadosis_process_ready_binding(
        ctx.block.chain_id,
        ctx.block.block_number,
        ctx.block.timestamp,
    );
    commit_transition::<MetadosisCycleLifecycle, _>(ctx.storage.clone(), binding, |_| {
        crate::runtime::start_metadosis(ctx)
    })
}

/// Purpose-bound terminal-sink adapter used by the Cycle-owned daily
/// allocation flow.
pub fn apply_cycle_day_limit(ctx: &BlockRuntimeContext<'_>, amount: U256) -> Result<U256> {
    crate::emission_sink::apply(ctx, amount)?;
    Ok(U256::ZERO)
}

/// Purpose-bound late-settlement adapter. Late residue is a Promis Limit credit only.
pub fn apply_late_settlement_headroom(ctx: &BlockRuntimeContext<'_>, amount: U256) -> Result<U256> {
    crate::emission_sink::apply_late_settlement_headroom(ctx, amount)
}

/// Sends the daily Cycle terminal allocation to the Metadosis sink, anchored at
/// `timestamp`.
pub fn dispatch_terminal_remainder_at(
    ctx: &BlockRuntimeContext<'_>,
    amount: U256,
    timestamp: u64,
) -> Result<()> {
    let mut terminal_block = ctx.block.clone();
    terminal_block.timestamp = timestamp;
    let terminal_ctx = BlockRuntimeContext::new(terminal_block, ctx.storage.clone());
    let unused = apply_cycle_day_limit(&terminal_ctx, amount)?;
    if !unused.is_zero() {
        return Err(
            outbe_emission::errors::EmissionLimitError::TerminalSinkReturnedUnusedAmount.into(),
        );
    }
    Ok(())
}

/// Recycles a late residue, from fee settlement or from a finished WAA or SRA
/// reward pool, into terminal Metadosis headroom.
pub fn dispatch_late_settlement_residue_at(
    ctx: &BlockRuntimeContext<'_>,
    amount: U256,
    timestamp: u64,
) -> Result<()> {
    if amount.is_zero() {
        return Ok(());
    }

    let mut terminal_block = ctx.block.clone();
    terminal_block.timestamp = timestamp;
    let terminal_ctx = BlockRuntimeContext::new(terminal_block, ctx.storage.clone());
    let unused = apply_late_settlement_headroom(&terminal_ctx, amount)?;
    if !unused.is_zero() {
        return Err(
            outbe_emission::errors::EmissionLimitError::LateSettlementSinkReturnedUnusedAmount
                .into(),
        );
    }
    Ok(())
}

pub fn advance_active_worldwide_days(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    let binding = metadosis_advance_due_binding(
        ctx.block.chain_id,
        ctx.block.block_number,
        ctx.block.timestamp,
    );
    commit_transition::<MetadosisCycleLifecycle, _>(ctx.storage.clone(), binding, |_| {
        crate::runtime::advance_active_worldwide_days(ctx)
    })
}
