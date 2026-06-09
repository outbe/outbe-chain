use alloy_primitives::Address;

use crate::{
    error::Result,
    storage::{StorageBacked, StorageHandle},
};

/// Runtime context shared by begin-block/end-block handlers.
///
/// The lifecycle executor builds this context from canonical block/header,
/// chain, and validator-set state before calling runtime modules.
#[derive(Clone, Debug, Default)]
pub struct BlockContext {
    pub block_number: u64,
    pub timestamp: u64,
    pub chain_id: u64,
    pub proposer: Address,
    pub validators: Vec<Address>,
}

impl BlockContext {
    pub fn new(
        block_number: u64,
        timestamp: u64,
        chain_id: u64,
        proposer: Address,
        validators: Vec<Address>,
    ) -> Self {
        Self {
            block_number,
            timestamp,
            chain_id,
            proposer,
            validators,
        }
    }

    pub fn empty_for_tests(block_number: u64, timestamp: u64, chain_id: u64) -> Self {
        Self::new(block_number, timestamp, chain_id, Address::ZERO, Vec::new())
    }
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
    fn begin_block(_ctx: &BlockRuntimeContext) -> Result<()> {
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<()> {
        Ok(())
    }
}
