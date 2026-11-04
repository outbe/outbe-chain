//! End-zone deletion sweep after value accounting.

use outbe_primitives::{block::BlockRuntimeContext, error::Result};

/// Deletes state after value accounting. Credis moves value during its call pass.
pub fn handle_cleanup_processing(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    outbe_gemfactory::hooks::cleanup(ctx)?;
    outbe_intexfactory::hooks::cleanup(ctx)?;
    outbe_gemfactory::hooks::cleanup_positions(ctx)?;
    Ok(())
}
