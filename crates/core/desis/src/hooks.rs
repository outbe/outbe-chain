//! Begin-block hook: drive the clearing fan-in gate.

use outbe_primitives::block::{BlockLifecycle, BlockRuntimeContext};
use outbe_primitives::error::Result;

pub struct DesisLifecycle;

impl BlockLifecycle for DesisLifecycle {
    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        crate::runtime::tick_gate(ctx)
    }
}
