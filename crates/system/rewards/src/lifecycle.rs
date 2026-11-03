//! Block lifecycle hook for the Rewards module.
//!
//! `RewardsLifecycle` is the zero-sized marker type that implements
//! [`BlockLifecycle`] and is registered in the executor's pre-execution
//! ordering. `begin_block` and `end_block` are no-ops. The start date is
//! the UTC date of the genesis timestamp on the block context.
//!
//! Day-boundary settle moved out of Rewards as part of the Cycle
//! refactor. The daily orchestration runs on
//! `outbe_cycle::runtime::handle_cycle_tick` and dispatches into EmissionLimit ->
//! AgentReward exactly once per UTC day.

use outbe_primitives::{block::BlockLifecycle, block::BlockRuntimeContext, error::Result};

/// Zero-sized marker implementing the block-lifecycle contract for the
/// Rewards module. Registered in
/// `outbe_evm::executor::run_outbe_pre_execution_hooks` so the executor
/// can keep ordering explicit and hard-fork governed
pub struct RewardsLifecycle;

impl BlockLifecycle for RewardsLifecycle {
    type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(_ctx: &BlockRuntimeContext) -> Result<()> {
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime;
    use crate::test_support::{block_ctx, with_block, GENESIS_TS as GENESIS_TS_2024_01_01};

    #[test]
    fn day_number_since_genesis_follows_the_genesis_timestamp() {
        with_block(0, GENESIS_TS_2024_01_01, |ctx| {
            assert_eq!(
                runtime::day_number_since_genesis(&ctx, 20240101).unwrap(),
                0
            );

            let ctx_later = BlockRuntimeContext::new(
                block_ctx(
                    100,
                    GENESIS_TS_2024_01_01 + 30 * outbe_primitives::time::SECONDS_PER_DAY,
                ),
                ctx.storage.clone(),
            );
            assert_eq!(
                runtime::day_number_since_genesis(&ctx_later, 20240101).unwrap(),
                0
            );
            assert_eq!(
                runtime::day_number_since_genesis(&ctx_later, 20240131).unwrap(),
                30
            );
        });
    }

    #[test]
    fn begin_block_is_idempotent_across_blocks() {
        with_block(0, GENESIS_TS_2024_01_01, |ctx0| {
            <RewardsLifecycle as BlockLifecycle>::begin_block(&ctx0).unwrap();

            let ctx1 = BlockRuntimeContext::new(
                block_ctx(1, GENESIS_TS_2024_01_01 + 60),
                ctx0.storage.clone(),
            );
            <RewardsLifecycle as BlockLifecycle>::begin_block(&ctx1).unwrap();

            let ctx_later = BlockRuntimeContext::new(
                block_ctx(
                    100,
                    GENESIS_TS_2024_01_01 + 30 * outbe_primitives::time::SECONDS_PER_DAY,
                ),
                ctx0.storage.clone(),
            );
            <RewardsLifecycle as BlockLifecycle>::begin_block(&ctx_later).unwrap();
        });
    }
}
