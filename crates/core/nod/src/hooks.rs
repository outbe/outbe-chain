//! Daily Nod call and forfeit hook.
//!
//! Qualification is derived when read (`api::is_qualified`).

use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

/// Daily cycle-trigger entry. Opens the day's call sweep and runs its first
/// slice. Later CycleTicks carry the remainder on through [`continue_sweeps`].
pub fn run_daily(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<()> {
    crate::called::scan_and_call(ctx, scope, parent)?;
    Ok(())
}

/// Runs from CycleTick every block, before the daily trigger can queue a newer day.
pub fn continue_sweeps(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<()> {
    crate::called::run_call_slice(ctx, scope, parent)?;
    Ok(())
}
