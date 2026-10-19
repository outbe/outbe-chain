//! Per-block entry points of the Intex sweeps, run from CycleTick.

use outbe_primitives::{block::BlockRuntimeContext, error::Result};

/// Expires the called groups whose notice period lapsed.
pub fn sweep_forfeits(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::expired::sweep_expired(ctx)?;
    Ok(())
}

/// Deletes the spent series of the expired groups.
pub fn sweep_cleanup(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::cleanup::sweep_cleanup(ctx)?;
    Ok(())
}
