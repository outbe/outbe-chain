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

use outbe_compressed_entities::{ExecutionScope, ParentBodySource, ParentBodySourceRef};
use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
};

/// Zero-sized marker registered in `outbe_evm::executor` begin-block ordering.
pub struct CycleLifecycle;

/// Explicit body authorities required by the Cycle block boundary.
pub struct CycleLifecycleContext<'a, 'storage> {
    pub runtime: BlockRuntimeContext<'storage>,
    pub scope: &'a ExecutionScope,
    parent: ParentBodySourceRef<'a>,
}

impl<'a, 'storage> CycleLifecycleContext<'a, 'storage> {
    #[must_use]
    pub fn new(
        runtime: BlockRuntimeContext<'storage>,
        scope: &'a ExecutionScope,
        parent: &'a dyn ParentBodySource,
    ) -> Self {
        Self {
            runtime,
            scope,
            parent: ParentBodySourceRef::new(parent),
        }
    }
}

impl BlockLifecycle for CycleLifecycle {
    type Context<'a, 'storage> = CycleLifecycleContext<'a, 'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &Self::Context<'_, '_>) -> Result<()> {
        if ctx.runtime.block.block_number == 1 {
            outbe_metadosis::runtime::init_genesis_day(&ctx.runtime)?;
        }
        crate::runtime::dispatch_triggers(&ctx.runtime, ctx.scope, &ctx.parent)
    }

    fn end_block(_ctx: &Self::Context<'_, '_>) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}
