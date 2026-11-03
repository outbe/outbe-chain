use alloy_primitives::{Address, B256};

use crate::{
    error::{PrecompileError, Result},
    storage::{StorageBacked, StorageHandle},
};

/// Error returned when a production block context is constructed with `genesis_timestamp == 0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("genesis_timestamp must be non-zero")]
pub struct ZeroGenesisTimestamp;

impl From<ZeroGenesisTimestamp> for PrecompileError {
    fn from(_: ZeroGenesisTimestamp) -> Self {
        PrecompileError::fatal("genesis_timestamp must be non-zero")
    }
}

/// The lifecycle executor builds this context from canonical block/header,
/// chain, and validator-set state before calling runtime modules.
#[derive(Clone, Debug)]
pub struct BlockContext {
    pub block_number: u64,
    pub timestamp: u64,
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub genesis_timestamp: u64,
    pub proposer: Address,
    pub validators: Vec<Address>,
}

#[derive(Clone)]
pub struct BlockRuntimeContext<'storage> {
    pub block: BlockContext,
    pub storage: StorageHandle<'storage>,
}

impl<'storage> BlockRuntimeContext<'storage> {
    pub fn new(block: BlockContext, storage: StorageHandle<'storage>) -> Self {
        Self { block, storage }
    }

    pub fn contract<C: StorageBacked<'storage>>(&self) -> C {
        self.storage.contract::<C>()
    }

    pub fn contract_at<C: StorageBacked<'storage>>(&self, address: Address) -> C {
        self.storage.contract_at::<C>(address)
    }

    pub fn with_checkpoint<R>(&self, f: impl FnOnce() -> Result<R>) -> Result<R> {
        self.storage.with_checkpoint(f)
    }
}

/// Static lifecycle contract for deterministic block-boundary runtime modules.
///
/// Implementations should be zero-sized marker types. The executor keeps the
/// ordering explicit and calls implementations through this trait instead of
/// passing ad hoc `(timestamp, block_number, ...)` argument lists.
pub trait BlockLifecycle {
    /// Typed runtime authority required by this lifecycle. Ordinary modules
    /// use [`BlockRuntimeContext`]; modules that need additional explicit
    /// capabilities wrap it in a dedicated context instead of bypassing the
    /// lifecycle contract with an ad-hoc entrypoint.
    type Context<'a, 'storage>;

    /// Value produced after all lifecycle-owned end-block work succeeds.
    ///
    /// Lifecycles without an associated output use `()`.
    type EndBlockResult;

    fn begin_block(_ctx: &Self::Context<'_, '_>) -> Result<()> {
        Ok(())
    }

    fn end_block(ctx: &Self::Context<'_, '_>) -> Result<Self::EndBlockResult>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::hashmap::HashMapStorageProvider;

    struct OutputLifecycle;

    impl BlockLifecycle for OutputLifecycle {
        type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
        type EndBlockResult = u64;

        fn end_block(ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
            Ok(ctx.block.block_number)
        }
    }

    #[test]
    fn lifecycle_end_block_can_return_an_associated_output() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext {
                    block_number: 7,
                    timestamp: 11,
                    chain_id: 1,
                    genesis_hash: B256::ZERO,
                    genesis_timestamp: 11,
                    proposer: Address::ZERO,
                    validators: Vec::new(),
                },
                storage,
            );

            assert_eq!(OutputLifecycle::end_block(&ctx).unwrap(), 7);
        });
    }
}
