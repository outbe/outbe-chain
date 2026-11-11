use outbe_primitives::{
    block::{BlockRuntimeContext, ProtocolModule},
    error::Result,
    storage::StorageHandle,
};

use crate::contract::Staking;

/// Protocol module for the begin-block staking step.
pub struct StakingModule;

impl ProtocolModule for StakingModule {
    const NAME: &'static str = "staking";

    fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        process_unbonding(ctx.storage.clone(), ctx.block.timestamp)
    }
}

/// Called from pre-execution.
///
/// Does not zero mature entries. `claim_unbonded` zeroes them.
/// Compaction is a capped tail trim, not a swap-remove.
/// The call also moves the stake of an UNBONDING validator into the queue.
pub fn process_unbonding(storage: StorageHandle, timestamp: u64) -> Result<()> {
    let mut staking = Staking::new(storage);
    staking.process_unbonding(timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::block::BlockContext;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;

    #[test]
    fn staking_module_begin_block_runs_process_unbonding() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext {
                    block_gas_limit: 0,
                    user_gas_used: 0,
                    block_number: 1,
                    timestamp: 1_700_000_000,
                    chain_id: 1,
                    genesis_hash: alloy_primitives::B256::ZERO,
                    genesis_timestamp: 1_700_000_000,
                    proposer: alloy_primitives::Address::ZERO,
                    validators: Vec::new(),
                },
                storage,
            );
            StakingModule::begin_block(&ctx).unwrap();
        });
    }
}
