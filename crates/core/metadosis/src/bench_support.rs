//! Narrow benchmark-only adapters over production Metadosis transitions.

use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    error::Result,
    storage::StorageHandle,
};
use outbe_system_common::time::WorldwideDay;

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
        BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number,
            timestamp,
            chain_id,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: u64::max(timestamp, 1),
            proposer: alloy_primitives::Address::ZERO,
            validators: Vec::new(),
        },
        storage.clone(),
    );
    let binding = outbe_system_common::metadosis_bindings::metadosis_process_ready_binding(
        storage.chain_id()?,
        storage.block_number()?,
        storage.now_secs()?,
    );
    storage.with_scoped_mutation::<outbe_primitives::storage::CycleLifecyclePurpose, _>(
        binding,
        || {
            outbe_tributefactory::worldwideday::create_worldwide_day_for_date(
                &context,
                worldwide_day,
            )
        },
    )
}
