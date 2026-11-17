//! Block lifecycle hook for proposal tally.

use outbe_primitives::block::{BlockRuntimeContext, ProtocolModule};
use outbe_primitives::error::Result;

use crate::handlers::{self, VoteTargetRegistry};
use crate::schema::Vote;

/// Protocol module for the begin-block vote step.
pub struct VoteModule;

impl ProtocolModule for VoteModule {
    const NAME: &'static str = "vote";

    /// Tallies the closed proposals, then applies the approved actions whose
    /// execution delay ended.
    fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        VoteLifecycle::begin_block_with_handlers(ctx, handlers::registry())?;
        handlers::apply_due_approvals(&ctx.storage, ctx.block.block_number)
    }
}

/// Lifecycle hooks for vote runtime processing.
pub struct VoteLifecycle;

impl VoteLifecycle {
    /// Tally expired proposals and dispatch approved ones at the current block.
    ///
    /// In production, the registry is [`crate::handlers::registry()`].
    pub fn begin_block_with_handlers(
        ctx: &BlockRuntimeContext,
        registry: &VoteTargetRegistry,
    ) -> Result<()> {
        let mut governance = Vote::new(ctx.storage.clone());
        governance.process_begin_block(ctx, registry)
    }
}
