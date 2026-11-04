//! End-zone walk of scheduled call, forfeit, and notice work.

use outbe_primitives::{block::BlockRuntimeContext, error::Result};

/// Each factory finishes calls before forfeits for its pinned closed day.
/// Gem includes position expiry. Credis includes collateral-return cleanup.
/// A gas stop commits the prefix and resumes next block.
pub fn handle_call_event_processing(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    outbe_nodfactory::called::process_call_pass(ctx)?;
    outbe_gemfactory::called::process_call_pass(ctx)?;
    outbe_intexfactory::called::process_call_pass(ctx)?;
    outbe_credisfactory::called::process_call_pass(ctx)?;
    outbe_intexfactory::notify::send_call_notices(ctx)?;
    Ok(())
}
