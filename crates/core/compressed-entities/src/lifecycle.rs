use outbe_primitives::{
    block::{BlockContext, BlockLifecycle, BlockRuntimeContext},
    error::Result,
    storage::StorageHandle,
};

use crate::{api::ExecutionScope, state::State};

/// Zero-sized storage-lifecycle marker used by the explicit executor ordering.
pub struct CompressedEntitiesLifecycle;

/// Explicit authorities consumed by the compressed-entity block boundary.
pub struct CompressedEntitiesLifecycleContext<'a, 'storage> {
    pub runtime: BlockRuntimeContext<'storage>,
    pub scope: &'a ExecutionScope,
}

impl<'a, 'storage> CompressedEntitiesLifecycleContext<'a, 'storage> {
    #[must_use]
    pub fn new(runtime: BlockRuntimeContext<'storage>, scope: &'a ExecutionScope) -> Self {
        Self { runtime, scope }
    }
}

impl BlockLifecycle for CompressedEntitiesLifecycle {
    type Context<'a, 'storage> = CompressedEntitiesLifecycleContext<'a, 'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &Self::Context<'_, '_>) -> Result<()> {
        begin_storage(ctx.runtime.storage.clone())?;
        ctx.scope.activate()
    }

    fn end_block(ctx: &Self::Context<'_, '_>) -> Result<Self::EndBlockResult> {
        ctx.scope.require_active()?;
        // Close the executor capability before cleanup so no later read can
        // fall through to the finalized parent while temporary state is removed.
        ctx.scope.finish()?;
        end_storage(ctx.runtime.storage.clone())
    }
}

pub(crate) fn begin_block(storage: StorageHandle<'_>, scope: &ExecutionScope) -> Result<()> {
    let runtime = BlockRuntimeContext::new(BlockContext::default(), storage);
    let lifecycle = CompressedEntitiesLifecycleContext::new(runtime, scope);
    <CompressedEntitiesLifecycle as BlockLifecycle>::begin_block(&lifecycle)
}

pub(crate) fn end_block(storage: StorageHandle<'_>, scope: &ExecutionScope) -> Result<()> {
    let runtime = BlockRuntimeContext::new(BlockContext::default(), storage);
    let lifecycle = CompressedEntitiesLifecycleContext::new(runtime, scope);
    <CompressedEntitiesLifecycle as BlockLifecycle>::end_block(&lifecycle)
}

fn begin_storage(storage: StorageHandle<'_>) -> Result<()> {
    State::new(storage).assert_clean_begin()
}

fn end_storage(storage: StorageHandle<'_>) -> Result<()> {
    storage
        .clone()
        .with_checkpoint(|| State::new(storage).cleanup())
}
