//! Per-block entry points of the Credis sweeps, run from CycleTick.

use outbe_primitives::{block::BlockRuntimeContext, error::Result};

/// Voids the called positions whose settlement window lapsed.
pub fn sweep_forfeits(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::expired::sweep_expired(ctx)?;
    Ok(())
}

/// One block of every Credis sweep: what fell due, then a slice of the call sweep.
pub fn continue_sweeps(ctx: &BlockRuntimeContext) -> Result<()> {
    sweep_forfeits(ctx)?;
    crate::called::run_call_slice(ctx)?;
    Ok(())
}
