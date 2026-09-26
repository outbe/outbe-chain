//! Begin-block carry-on for the Intex sweeps and drains.

use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
};

pub struct IntexLifecycle;

impl BlockLifecycle for IntexLifecycle {
    type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        // A call sweep the daily trigger could not finish in one go carries on
        // here, block by block, rather than waiting a day for the next trigger.
        crate::called::run_call_slice(ctx)?;
        // Drain in-flight payouts first, then start rounds for any series whose
        // proceeds fan-in deadline has passed.
        crate::runtime::drain_distributions(&ctx.storage)?;
        crate::runtime::sweep_proceeds_deadlines(&ctx.storage, ctx.block.timestamp)?;
        crate::expired::sweep_expiry_deadlines(ctx)?;
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}
