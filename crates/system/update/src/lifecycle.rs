//! Block lifecycle hook for scheduled update activation.

use outbe_primitives::block::{BlockRuntimeContext, ProtocolModule};
use outbe_primitives::error::Result;

use crate::handlers::{self, UpgradeHandlerRegistry};
use crate::schema::Update;

/// Protocol module for the begin-block update step.
pub struct UpdateModule;

impl ProtocolModule for UpdateModule {
    const NAME: &'static str = "update";

    fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        UpdateLifecycle::begin_block_with_handlers(ctx, handlers::registry())
    }
}

/// Lifecycle hooks for runtime processing.
pub struct UpdateLifecycle;

impl UpdateLifecycle {
    /// Activates each scheduled update when the block height reaches its activation height.
    /// Vote owns proposal tally. This hook does not tally proposals.
    ///
    /// In production, [`UpdateModule::begin_block`] invokes this with
    /// [`crate::handlers::registry()`].
    pub fn begin_block_with_handlers(
        ctx: &BlockRuntimeContext,
        registry: &UpgradeHandlerRegistry,
    ) -> Result<()> {
        let mut update = Update::new(ctx.storage.clone());
        update.process_begin_block_with_handlers(ctx, registry)
    }
}
