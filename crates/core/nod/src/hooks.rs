//! Daily Nod call and forfeit hook.
//!
//! Each closed UTC day is pinned when the `NodDaily` trigger opens a sweep.
//! Later CycleTicks continue the same day with the same prices, so a later UTC
//! rollover cannot reprice the remainder. Qualification is not swept: it is
//! derived from finalized daily VWAPs when it is read (`api::is_qualified`).

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

/// Advance an in-flight call sweep by one slice. Runs from CycleTick on every
/// block, before the daily trigger can queue a newer day, so an unfinished walk
/// keeps the prices it opened with.
pub fn continue_sweeps(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<()> {
    crate::called::run_call_slice(ctx, scope, parent)?;
    Ok(())
}
