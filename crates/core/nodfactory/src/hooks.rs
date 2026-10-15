//! Per-block entry points of the Nod sweeps, run from CycleTick.

use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

/// Burns the unpaid Nods of the called buckets whose notice period lapsed.
pub fn sweep_forfeits(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<()> {
    crate::expired::sweep_expired(ctx, scope, parent)?;
    Ok(())
}
