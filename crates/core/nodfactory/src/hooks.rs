//! Per-block entry points of the Nod sweeps, run from CycleTick.

use outbe_primitives::{block::BlockRuntimeContext, error::Result};

/// Burns the unpaid Nods of the called groups whose notice period lapsed.
pub fn sweep_forfeits(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::expired::sweep_expired(ctx)?;
    Ok(())
}
