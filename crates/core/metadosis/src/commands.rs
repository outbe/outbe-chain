use outbe_primitives::{block::BlockRuntimeContext, error::Result, storage::CycleLifecyclePurpose};
use outbe_system_common::metadosis_bindings::metadosis_process_ready_binding;

use crate::commit::commit_transition;

/// Starts Metadosis of emission day `day` once the offering of the day ended.
pub fn start_metadosis(ctx: &BlockRuntimeContext<'_>, day: u32) -> Result<()> {
    let binding = metadosis_process_ready_binding(
        ctx.block.chain_id,
        ctx.block.block_number,
        ctx.block.timestamp,
    );
    commit_transition::<CycleLifecyclePurpose, _>(ctx.storage.clone(), binding, |_| {
        crate::runtime::start_metadosis(ctx, day)
    })
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
