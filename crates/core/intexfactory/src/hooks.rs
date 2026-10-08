//! Per-block entry points of the Intex sweeps, run from CycleTick.

use outbe_primitives::{block::BlockRuntimeContext, error::Result};

/// Opens the payout rounds whose proceeds deadline passed.
pub fn sweep_proceeds(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::runtime::sweep_proceeds_deadlines(&ctx.storage, ctx.block.timestamp)
}

/// Expires the called groups whose notice period lapsed.
pub fn sweep_forfeits(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::expired::sweep_expired(ctx)
}

/// One block of every Intex sweep: what fell due, a slice of the call sweep, and the
/// notices it queued.
pub fn continue_sweeps(ctx: &BlockRuntimeContext) -> Result<()> {
    sweep_proceeds(ctx)?;
    sweep_forfeits(ctx)?;
    crate::called::run_call_slice(ctx)?;
    crate::notify::send_notices(ctx)
}
