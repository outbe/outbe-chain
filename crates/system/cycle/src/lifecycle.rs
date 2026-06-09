//! Block lifecycle hook for the Cycle dispatcher.
//!
//! moves CycleTick into begin-block system-transaction semantics.
//! Phase 1 applies the immediate parent's finalization facts first; Phase 2
//! then runs `CycleLifecycle::begin_block` so UTC-day settlement observes the
//! complete previous-day bucket before user transactions execute.
//!
//! The dispatcher itself is fully idempotent per slot via
//! `Cycle.last_executed_at[trigger_id]`, so it is safe to invoke on
//! every block — the daily trigger only does work on the first block
//! whose timestamp crosses the next UTC midnight.

use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
};

/// Zero-sized marker registered in `outbe_evm::executor` begin-block ordering.
pub struct CycleLifecycle;

impl BlockLifecycle for CycleLifecycle {
    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        // Block 1 genesis bootstrap: create the first metadosis worldwide day.
        // The daily Cycle trigger only anchors (does not fire) on its first
        // encounter, so it never invokes `start_metadosis` at block 1 — the
        // genesis day must be created here, before user transactions. Idempotent.
        if ctx.block.block_number == 1 {
            outbe_metadosis::runtime::init_genesis_day(ctx)?;
        }
        crate::runtime::dispatch_triggers(ctx)
    }
}
