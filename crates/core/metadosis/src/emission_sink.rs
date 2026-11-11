use alloy_primitives::U256;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    storage::{metadosis_cycle_allocation_binding, MetadosisCycleLifecycle},
    time::{date_key_to_utc_timestamp, timestamp_to_date_key},
};

use crate::precompile::IMetadosis;
use crate::schema::MetadosisContract;

/// Writes or updates the emission allocation for the target UTC day in `EmissionDay`.
pub(crate) fn apply(ctx: &BlockRuntimeContext, amount: U256) -> Result<()> {
    let day_ts = date_key_to_utc_timestamp(timestamp_to_date_key(ctx.block.timestamp))
        .map_err(crate::lifecycle::date_step_error)?;
    let binding =
        metadosis_cycle_allocation_binding(ctx.block.chain_id, ctx.block.block_number, day_ts);
    crate::commit::commit_transition::<MetadosisCycleLifecycle, _>(
        ctx.storage.clone(),
        binding,
        |_| apply_day_limit(ctx, amount),
    )
}

fn apply_day_limit(ctx: &BlockRuntimeContext, base_limit: U256) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    let utc_day = timestamp_to_date_key(ctx.block.timestamp);
    crate::lifecycle::ensure_emission_day_with_limit(&mut metadosis, ctx, utc_day, base_limit)?;

    metadosis.emit(IMetadosis::MetadosisLimitSet {
        emissionDay: utc_day,
        metadosisLimitMinor: base_limit,
    })
}
