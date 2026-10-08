//! Nod call and forfeit sweep entry points.
//!
//! Qualification is derived when read (`api::is_qualified`).

use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

/// Daily cycle-trigger entry. Opens the day's call sweep and runs its first
/// slice. Later CycleTicks carry the remainder on through [`continue_sweeps`].
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::called::schedule(ctx)
}

/// Runs from CycleTick every block, before the daily trigger can queue a newer day.
/// Burns the lapsed called buckets after the call slice.
pub fn continue_sweeps(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<()> {
    crate::called::run_call_slice(ctx)?;
    crate::called::sweep_expired(ctx, scope, parent)?;
    Ok(())
}
