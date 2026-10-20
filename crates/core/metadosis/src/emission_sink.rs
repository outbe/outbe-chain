use alloy_primitives::U256;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    storage::{
        metadosis_cycle_allocation_binding, metadosis_late_settlement_binding,
        MetadosisCertifiedFinality, MetadosisCycleLifecycle,
    },
    time::timestamp_to_date_key,
};
use outbe_promislimit::PromisLimitContract;

use crate::precompile::IMetadosis;
use crate::schema::MetadosisContract;

/// Writes or updates the emission allocation for the target UTC day in `EmissionDay`.
pub(crate) fn apply(ctx: &BlockRuntimeContext, amount: U256) -> Result<()> {
    let binding = metadosis_cycle_allocation_binding(
        ctx.block.chain_id,
        ctx.block.block_number,
        ctx.block.timestamp,
    );
    crate::commit::commit_transition::<MetadosisCycleLifecycle, _>(
        ctx.storage.clone(),
        binding,
        |_| apply_day_limit(ctx, amount),
    )
}

/// Recycles late-settlement headroom into PromisLimit carry-over.
pub(crate) fn apply_late_settlement_headroom(
    ctx: &BlockRuntimeContext,
    amount: U256,
) -> Result<U256> {
    let binding = metadosis_late_settlement_binding(
        ctx.block.chain_id,
        ctx.block.block_number,
        ctx.block.timestamp,
    );
    crate::commit::commit_transition::<MetadosisCertifiedFinality, _>(
        ctx.storage.clone(),
        binding,
        |_| {
            PromisLimitContract::new(ctx.storage.clone()).checked_add_carry_over(amount)?;
            Ok(U256::ZERO)
        },
    )
}

fn apply_day_limit(ctx: &BlockRuntimeContext, base_limit: U256) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    let utc_day = timestamp_to_date_key(ctx.block.timestamp);
    crate::lifecycle::ensure_emission_day_with_limit(&mut metadosis, ctx, utc_day, base_limit)?;

    let carry_over = PromisLimitContract::new(ctx.storage.clone()).get_total_unallocated()?;
    metadosis.emit(IMetadosis::MetadosisAccumulation {
        date: utc_day,
        metadosisLimitMinor: base_limit,
        totalAccumulatedMinor: base_limit,
        blockNumber: ctx.block.block_number,
    })?;
    metadosis.emit(IMetadosis::DayLimitFormed {
        worldwideDay: utc_day,
        baseLimitMinor: base_limit,
        promisLimitBeforeMinor: carry_over,
        promisLimitTakenMinor: U256::ZERO,
        promisLimitAfterMinor: carry_over,
        metadosisLimitMinor: base_limit,
        blockNumber: ctx.block.block_number,
    })
}
