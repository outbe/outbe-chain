//! Begin-block hook: drive the clearing fan-in gate.

use outbe_primitives::block::{BlockLifecycle, BlockRuntimeContext};
use outbe_primitives::error::Result;

pub struct DesisLifecycle;

impl BlockLifecycle for DesisLifecycle {
    type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        crate::runtime::tick_gate(ctx)
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}
