use alloy_primitives::U256;
use outbe_primitives::{block::BlockRuntimeContext, error::Result, storage::CycleLifecyclePurpose};
use outbe_system_common::metadosis_bindings::metadosis_process_ready_binding;

use crate::commit::commit_transition;

/// Runs the Metadosis of emission day `day`, which the Cycle allocated in this run.
pub fn process_emission_day(ctx: &BlockRuntimeContext<'_>, day: u32) -> Result<()> {
    let binding = metadosis_process_ready_binding(
        ctx.block.chain_id,
        ctx.block.block_number,
        ctx.block.timestamp,
    );
    commit_transition::<CycleLifecyclePurpose, _>(ctx.storage.clone(), binding, |_| {
        crate::runtime::process_emission_day(ctx, day)
    })
}

/// Gives Metadosis of `day` its limit, before the Cycle marks the record of `day` Processing.
pub fn set_metadosis_limit(ctx: &BlockRuntimeContext<'_>, day: u32, amount: U256) -> Result<()> {
    crate::limits::set_metadosis_limit(ctx, day, amount)
}

/// Marks Metadosis of `day` as `Failed` (returning the part of its Metadosis limit that the
/// loads do not take to `PromisLimit` once and emitting `IMetadosis.MetadosisFailed(day, 1)`).
pub fn fail_metadosis(ctx: &BlockRuntimeContext<'_>, day: u32) -> Result<()> {
    let mut metadosis = crate::schema::MetadosisContract::new(ctx.storage.clone());
    crate::lifecycle::fail_metadosis(
        &mut metadosis,
        day,
        crate::constants::METADOSIS_FAILED_STEP_REVERTED,
    )
}
