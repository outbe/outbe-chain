//! Per-block entry points of the Credis sweeps, run from CycleTick.

use outbe_primitives::{block::BlockRuntimeContext, error::Result};

/// Forfeits the called groups whose settlement window lapsed.
pub fn sweep_forfeits(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::expired::sweep_expired(ctx)?;
    Ok(())
}

/// Deletes the Credis of the forfeited groups.
pub fn sweep_cleanup(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::cleanup::sweep_cleanup(ctx)?;
    Ok(())
}
