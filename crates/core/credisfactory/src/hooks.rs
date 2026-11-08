//! Block-boundary protocol module for `CredisFactory`.

use outbe_primitives::{
    block::{BlockRuntimeContext, ProtocolModule},
    error::Result,
};

/// Protocol module for `SystemTxKind::CredisFactory` (`OSCR`).
pub struct CredisFactoryModule;

impl ProtocolModule for CredisFactoryModule {
    const NAME: &'static str = "credis_factory";

    fn end_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        crate::called::process_call_pass(ctx)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::block::BlockContext;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    #[test]
    fn credis_factory_module_end_block_runs_call_pass() {
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
            <CredisFactoryModule as ProtocolModule>::end_block(&ctx).unwrap();
        });
    }
}
