//! Block lifecycle hook for scheduled update activation.

use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;

use crate::handlers::UpgradeHandlerRegistry;
use crate::schema::Update;

/// Lifecycle hooks for runtime processing.
pub struct UpdateLifecycle;

impl UpdateLifecycle {
    /// Activates each scheduled update when the block height reaches its activation height.
    /// Vote owns proposal tally. This hook does not tally proposals.
    ///
    /// Unlike other lifecycle modules, Update does not implement
    /// [`BlockLifecycle`](outbe_primitives::block::BlockLifecycle) directly. Callers
    /// must pass the node-level upgrade handler registry explicitly. In production,
    /// this is `outbe_evm::handlers::update::registry()`.
    ///
    /// The registry is owned outside `outbe-update` so migration handlers can live
    /// in their owning crates without creating a dependency cycle.
    pub fn begin_block_with_handlers(
        ctx: &BlockRuntimeContext,
        registry: &UpgradeHandlerRegistry,
    ) -> Result<()> {
        let mut update = Update::new(ctx.storage.clone());
        update.process_begin_block_with_handlers(ctx, registry)
    }
}
