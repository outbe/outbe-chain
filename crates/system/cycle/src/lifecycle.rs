//! Block lifecycle hook for the Cycle dispatcher.
//!
//! moves CycleTick into begin-block system-transaction semantics.
//! Phase 1 applies the immediate parent's finalization facts first; Phase 2
//! then runs `CycleLifecycle::begin_block` so UTC-day settlement observes the
//! complete previous-day bucket before user transactions execute.
//!
//! The dispatcher itself is fully idempotent per slot via
//! `Cycle.last_executed_at[trigger_id]`, so it is safe to invoke on
//! every block — the daily trigger only does work on the first block
//! whose timestamp crosses the next UTC midnight.

use outbe_nod::NodRepositoryReader;
use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
};
use outbe_tribute::TributeRepositoryReader;

/// Zero-sized marker registered in `outbe_evm::executor` begin-block ordering.
pub struct CycleLifecycle;

impl CycleLifecycle {
    /// Runs the production Cycle tick with explicit least-authority body readers.
    pub fn begin_block_with_readers(
        ctx: &BlockRuntimeContext,
        tribute_bodies: &TributeRepositoryReader,
        nod_bodies: &NodRepositoryReader,
    ) -> Result<()> {
        if ctx.block.block_number == 1 {
            outbe_metadosis::runtime::init_genesis_day(ctx)?;
        }
        crate::runtime::dispatch_triggers(ctx, tribute_bodies, nod_bodies)
    }
}

impl BlockLifecycle for CycleLifecycle {
    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        #[cfg(test)]
        {
            use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle};
            use std::sync::Arc;

            let storage: StorageReaderHandle = Arc::new(MemoryStorage::new());
            let tribute_bodies = TributeRepositoryReader::new(storage.clone());
            let nod_bodies = NodRepositoryReader::new(storage);
            Self::begin_block_with_readers(ctx, &tribute_bodies, &nod_bodies)
        }

        #[cfg(not(test))]
        {
            let _ = ctx;
            Err(outbe_primitives::error::PrecompileError::Fatal(
                "Cycle execution body read authority was not supplied".into(),
            ))
        }
    }
}
