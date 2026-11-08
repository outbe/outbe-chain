//! Entry points of the Nod passes.

use outbe_primitives::block::{BlockRuntimeContext, ProtocolModule};
use outbe_primitives::error::Result;

/// Deletes the unpaid Nods of the forfeited groups.
pub use crate::cleanup::cleanup;

pub struct NodFactoryModule;

impl ProtocolModule for NodFactoryModule {
    const NAME: &'static str = "nod_factory";

    fn end_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        crate::called::process_call_pass(ctx)?;
        crate::cleanup::cleanup(ctx)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::block::BlockContext;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    #[test]
    fn nod_factory_module_end_block_runs_the_call_and_cleanup_passes() {
        assert_eq!(<NodFactoryModule as ProtocolModule>::NAME, "nod_factory");
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 10_000_000,
                user_gas_used: 0,
                block_number: 5,
                timestamp: 1_800_000_000,
                chain_id: 1,
                genesis_hash: alloy_primitives::B256::repeat_byte(0x01),
                genesis_timestamp: 1_800_000_000,
                proposer: alloy_primitives::Address::repeat_byte(0x02),
                validators: Vec::new(),
            },
            storage,
        );
        <NodFactoryModule as ProtocolModule>::end_block(&ctx).unwrap();
    }
}
