//! End-zone deletion sweep after value accounting.
//!
//! Nod runs first, then Gem, Intex, and Gem positions. Credis moves value during its call pass.

use outbe_primitives::{block::BlockRuntimeContext, error::Result};

/// Deletes already-accounted Nod, Gem, and Intex state. Credis moves value during its call pass.
pub fn handle_cleanup_processing(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    outbe_nodfactory::hooks::cleanup(ctx)?;
    outbe_gemfactory::hooks::cleanup(ctx)?;
    outbe_intexfactory::hooks::cleanup(ctx)?;
    outbe_gemfactory::hooks::cleanup_positions(ctx)?;
    Ok(())
}
