use alloy_primitives::U256;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    storage::{metadosis_process_ready_binding, MetadosisCycleLifecycle},
};

use crate::commit::commit_transition;

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

/// Sends the daily Cycle terminal allocation to the Metadosis sink, anchored at
/// `timestamp`.
pub fn set_metadosis_limit(
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

pub fn find_due_emission_day(
    ctx: &BlockRuntimeContext<'_>,
) -> Result<Option<crate::schema::EmissionDay>> {
    let metadosis = crate::schema::MetadosisContract::new(ctx.storage.clone());
    crate::lifecycle::find_due_emission_day(&metadosis, ctx)
}

/// Marks Metadosis of `day` as `Failed` (returning its Metadosis limit to `PromisLimit`
/// once and emitting `IMetadosis.MetadosisFailed(day, 1)`).
pub fn fail_metadosis(ctx: &BlockRuntimeContext<'_>, day: u32) -> Result<()> {
    let mut metadosis = crate::schema::MetadosisContract::new(ctx.storage.clone());
    crate::lifecycle::fail_metadosis(
        &mut metadosis,
        day,
        crate::constants::METADOSIS_FAILED_STEP_REVERTED,
    )
}

/// Emits `IMetadosis.MetadosisCanceled(day, 1)` after daily emission limit allocation reverts.
pub fn emit_canceled_after_failed_allocation(
    ctx: &BlockRuntimeContext<'_>,
    day: u32,
) -> Result<()> {
    crate::schema::MetadosisContract::new(ctx.storage.clone()).emit(
        crate::precompile::IMetadosis::MetadosisCanceled {
            emissionDay: day,
            reason: crate::constants::METADOSIS_CANCELED_NO_LIMIT,
        },
    )
}
