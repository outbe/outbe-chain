//! Entry points of the Gem passes.

use outbe_primitives::block::{BlockRuntimeContext, ProtocolModule};
use outbe_primitives::error::Result;

/// Deletes the unpaid gems of the forfeited groups.
pub use crate::cleanup::cleanup;

/// Deletes the positions of the expired hours.
pub use crate::positions_expired::cleanup_positions;

pub struct GemFactoryModule;

impl ProtocolModule for GemFactoryModule {
    const NAME: &'static str = "gem_factory";

    fn end_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        crate::called::process_call_pass(ctx)?;
        Ok(())
    }
}

pub struct GemCleanupModule;

impl ProtocolModule for GemCleanupModule {
    const NAME: &'static str = "gem_cleanup";

    fn end_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        crate::cleanup::cleanup(ctx)?;
        crate::positions_expired::cleanup_positions(ctx)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::block::BlockContext;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    #[test]
    fn gem_factory_and_cleanup_modules_run_end_block_passes() {
        assert_eq!(<GemFactoryModule as ProtocolModule>::NAME, "gem_factory");
        assert_eq!(<GemCleanupModule as ProtocolModule>::NAME, "gem_cleanup");
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let block = BlockContext {
                block_gas_limit: 30_000_000,
                user_gas_used: 0,
                block_number: 2,
                timestamp: 1_700_000_000,
                chain_id: 1,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: 1_700_000_000,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            };
            let ctx = BlockRuntimeContext::new(block, storage);
            <GemFactoryModule as ProtocolModule>::end_block(&ctx).unwrap();
            <GemCleanupModule as ProtocolModule>::end_block(&ctx).unwrap();
        });
    }
}
