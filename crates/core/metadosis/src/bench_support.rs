//! Narrow benchmark-only adapters over production Metadosis transitions.

use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    error::Result,
    storage::StorageHandle,
};

/// Creates one WWD through the canonical production transition without
/// exposing the raw Metadosis storage facade to the benchmark crate.
pub fn create_worldwide_day(
    storage: StorageHandle<'_>,
    block_number: u64,
    timestamp: u64,
    chain_id: u64,
    worldwide_day: WorldwideDay,
) -> Result<()> {
    let context = BlockRuntimeContext::new(
        BlockContext::empty_for_tests(block_number, timestamp, chain_id),
        storage.clone(),
    );
    let binding = outbe_primitives::storage::metadosis_process_ready_binding(
        storage.chain_id()?,
        storage.block_number()?,
        storage.now_secs()?,
    );
    storage.with_metadosis_mutation::<outbe_primitives::storage::MetadosisCycleLifecycle, _>(
        binding,
        || {
            crate::lifecycle::create_worldwide_day_for_date(
                &mut crate::schema::MetadosisContract::new(storage.clone()),
                &context,
                worldwide_day,
            )
        },
    )
}
