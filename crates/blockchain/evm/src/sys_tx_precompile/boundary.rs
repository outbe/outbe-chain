#[cfg(any(test, feature = "test-utils"))]
use crate::executor::apply_boundary_outcome;
#[cfg(any(test, feature = "test-utils"))]
use outbe_primitives::block::BlockRuntimeContext;
#[cfg(any(test, feature = "test-utils"))]
use outbe_primitives::consensus::DkgBoundaryArtifact;
#[cfg(any(test, feature = "test-utils"))]
use outbe_primitives::error::Result;

/// BoundaryOutcome system tx: activate a DKG/reshare boundary before user transactions.
#[cfg(any(test, feature = "test-utils"))]
pub(crate) fn run_boundary_outcome(
    ctx: &BlockRuntimeContext,
    artifact: &DkgBoundaryArtifact,
) -> Result<()> {
    apply_boundary_outcome(
        ctx.storage.clone(),
        artifact,
        ctx.block.block_number,
        ctx.block.timestamp,
    )?;
    crate::active_evm_runtime_host().sync_hyperlane_validators(ctx)?;
    Ok(())
}
