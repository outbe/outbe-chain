use alloy_primitives::U256;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    storage::{metadosis_cycle_allocation_binding, MetadosisCycleLifecycle},
    time::date_key_to_utc_timestamp,
};

use crate::precompile::IMetadosis;
use crate::schema::MetadosisContract;

/// Records that Metadosis of `day` holds `amount`. The Cycle writes the amount to the record of `day`.
pub(crate) fn apply(ctx: &BlockRuntimeContext, day: u32, amount: U256) -> Result<()> {
    let day_ts = date_key_to_utc_timestamp(day).map_err(crate::lifecycle::date_step_error)?;
    let binding =
        metadosis_cycle_allocation_binding(ctx.block.chain_id, ctx.block.block_number, day_ts);
    crate::commit::commit_transition::<MetadosisCycleLifecycle, _>(
        ctx.storage.clone(),
        binding,
        |_| {
            MetadosisContract::new(ctx.storage.clone()).emit(IMetadosis::MetadosisLimitSet {
                emissionDay: day,
                metadosisLimitMinor: amount,
            })
        },
    )
}
