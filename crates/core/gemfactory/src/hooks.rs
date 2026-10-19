//! Per-block entry points of the Gem sweeps, run from CycleTick.

use outbe_primitives::{block::BlockRuntimeContext, error::Result};

/// Forfeits the called groups whose notice period lapsed.
pub fn sweep_forfeits(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::expired::sweep_expired(ctx)?;
    Ok(())
}

/// Deletes the unpaid gems of the forfeited groups.
pub fn sweep_cleanup(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::cleanup::sweep_cleanup(ctx)?;
    Ok(())
}

/// Returns the unissued capacity of the hours whose positions expired.
pub fn sweep_expired_positions(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::positions_expired::sweep_expired_positions(ctx)?;
    Ok(())
}

/// Deletes the positions of the expired hours.
pub fn sweep_position_cleanup(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::positions_expired::sweep_position_cleanup(ctx)?;
    Ok(())
}
