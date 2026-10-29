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
            outbe_nodfactory::called::initialize_daily(&ctx.runtime)?;
            outbe_gemfactory::called::initialize_daily(&ctx.runtime)?;
            outbe_intexfactory::called::initialize_daily(&ctx.runtime)?;
            outbe_credisfactory::called::initialize_daily(&ctx.runtime)?;
        }
        // ProtocolCycle does not wait for multi-block factory passes.
        // Factories return capacity below. Subsequent reserve consumers can use this capacity.
        crate::runtime::dispatch_triggers(&ctx.runtime)?;
        crate::handler::credit_reward_slice(&ctx.runtime)?;
        // Each factory finishes calls before forfeits for its pinned closed day.
        // Gem includes position expiry. Credis includes collateral-return cleanup.
        outbe_nodfactory::called::process_call_pass(&ctx.runtime)?;
        outbe_gemfactory::called::process_call_pass(&ctx.runtime)?;
        outbe_intexfactory::called::process_call_pass(&ctx.runtime)?;
        outbe_credisfactory::called::process_call_pass(&ctx.runtime)?;
        outbe_intexfactory::notify::send_call_notices(&ctx.runtime)?;
        // These final sweeps only delete state after value accounting completes.
        // Credis moves value during cleanup, so its coupled pass above performs that cleanup instead.
        outbe_gemfactory::hooks::cleanup(&ctx.runtime)?;
        outbe_intexfactory::hooks::cleanup(&ctx.runtime)?;
        outbe_gemfactory::hooks::cleanup_positions(&ctx.runtime)?;
        Ok(())
    }

    fn end_block(_ctx: &Self::Context<'_, '_>) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}
