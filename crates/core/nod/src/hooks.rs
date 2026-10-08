//! Nod call and forfeit sweep entry points.
//!
//! Qualification is derived when read (`api::is_qualified`).

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

/// One block of every Nod sweep: what fell due, then a slice of the call sweep.
pub fn continue_sweeps(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<()> {
    sweep_forfeits(ctx, scope, parent)?;
    crate::called::run_call_slice(ctx)?;
    Ok(())
}
