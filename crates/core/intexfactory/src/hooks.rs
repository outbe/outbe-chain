//! Entry points of the Intex passes.

use outbe_primitives::{
    block::{BlockRuntimeContext, ProtocolModule},
    error::Result,
};

/// Deletes the spent series of the expired groups.
pub use crate::cleanup::cleanup;

/// Protocol module for `SystemTxKind::IntexFactory` (`OSIX`).
pub struct IntexFactoryModule;

impl ProtocolModule for IntexFactoryModule {
    const NAME: &'static str = "intex_factory";

    fn end_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        crate::called::process_call_pass(ctx)?;
        crate::notify::send_call_notices(ctx)?;
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
    fn intex_factory_module_end_block_runs_passes() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext {
                    block_gas_limit: 0,
                    user_gas_used: 0,
                    block_number: 2,
                    timestamp: 1_700_000_000,
                    chain_id: 1,
                    genesis_hash: alloy_primitives::B256::ZERO,
                    genesis_timestamp: 1_700_000_000,
                    proposer: alloy_primitives::Address::ZERO,
                    validators: Vec::new(),
                },
                storage,
            );
            <IntexFactoryModule as ProtocolModule>::end_block(&ctx).unwrap();
        });
    }
}
