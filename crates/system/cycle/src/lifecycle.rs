//! Block lifecycle hook for the Cycle dispatcher.
//!
//! This hook moves CycleTick into begin-block system-transaction semantics.
//! Phase 1 applies the immediate parent's finalization facts first. Phase 2
//! then runs `CycleLifecycle::begin_block`. At a UTC-day transition, settlement
//! waits until the previous day's canonical late-credit windows have executed.
//!
//! The dispatcher itself is fully idempotent per slot via
//! `Cycle.last_executed_at[trigger_id]`, so the caller can safely invoke it on
//! every block. ProtocolCycle runs on the first block after each UTC-hour
//! boundary, subject to that participation gate, and owns contiguous-day
//! settlement and missed-day forfeiture.

use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
};

/// Zero-sized marker registered in `outbe_evm::executor` begin-block ordering.
pub struct CycleLifecycle;

/// Explicit body authorities required by the Cycle block boundary.
pub struct CycleLifecycleContext<'storage> {
    pub runtime: BlockRuntimeContext<'storage>,

    metadosis_genesis_activation_height: u64,
}

impl<'storage> CycleLifecycleContext<'storage> {
    #[must_use]
    pub fn new(runtime: BlockRuntimeContext<'storage>) -> Self {
        Self {
            runtime,
            metadosis_genesis_activation_height: 1,
        }
    }

    /// Sets the block that initializes the native genesis WorldwideDay.
    /// Fresh genesis uses block 1.
    #[must_use]
    pub fn with_metadosis_genesis_activation_height(mut self, height: u64) -> Self {
        self.metadosis_genesis_activation_height = height;
        self
    }
}

impl BlockLifecycle for CycleLifecycle {
    type Context<'a, 'storage> = CycleLifecycleContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &Self::Context<'_, '_>) -> Result<()> {
        if ctx.runtime.block.block_number == ctx.metadosis_genesis_activation_height {
            outbe_metadosis::commands::init_genesis_day(&ctx.runtime)?;
        }
        // What fell due is settled first, so its returns reach the ProtocolCycle
        // that forms the day.
        outbe_nodfactory::hooks::sweep_forfeits(&ctx.runtime)?;
        outbe_gemfactory::hooks::sweep_forfeits(&ctx.runtime)?;
        outbe_intexfactory::hooks::sweep_forfeits(&ctx.runtime)?;
        outbe_credisfactory::hooks::sweep_forfeits(&ctx.runtime)?;
        crate::runtime::dispatch_triggers(&ctx.runtime)?;
        // The daily triggers only schedule. Each right walks one slice of its pinned day.
        outbe_nodfactory::called::run_call_slice(&ctx.runtime)?;
        outbe_gemfactory::called::run_call_slice(&ctx.runtime)?;
        outbe_intexfactory::called::run_call_slice(&ctx.runtime)?;
        outbe_credisfactory::called::run_call_slice(&ctx.runtime)?;
        outbe_intexfactory::notify::send_notices(&ctx.runtime)?;
        // Last: deleting what already expired moves no value and decides nothing.
        outbe_gemfactory::hooks::sweep_cleanup(&ctx.runtime)?;
        outbe_intexfactory::hooks::sweep_cleanup(&ctx.runtime)?;
        Ok(())
    }

    fn end_block(_ctx: &Self::Context<'_, '_>) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}
