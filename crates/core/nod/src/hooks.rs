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
