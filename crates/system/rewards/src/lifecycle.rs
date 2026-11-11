//! Block lifecycle hook for the Rewards module.
//!
//! `RewardsModule` implements [`ProtocolModule`] for the begin-block rewards step.
//! `begin_block` and `end_block` are no-ops. The start date is
//! the UTC date of the genesis timestamp on the block context.
//!
//! Day-boundary settle moved out of Rewards as part of the Cycle
//! refactor. The daily orchestration runs on
//! `outbe_cycle::runtime::handle_cycle_tick` and dispatches into EmissionLimit ->
//! AgentReward exactly once per UTC day.

use outbe_primitives::block::ProtocolModule;

/// Protocol module for the begin-block rewards step.
pub struct RewardsModule;

impl ProtocolModule for RewardsModule {
    const NAME: &'static str = "rewards";
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime;
    use crate::test_support::{block_ctx, with_block, GENESIS_TS as GENESIS_TS_2024_01_01};
    use outbe_primitives::block::BlockRuntimeContext;

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
            <RewardsModule as ProtocolModule>::begin_block(&ctx0).unwrap();

            let ctx1 = BlockRuntimeContext::new(
                block_ctx(1, GENESIS_TS_2024_01_01 + 60),
                ctx0.storage.clone(),
            );
            <RewardsModule as ProtocolModule>::begin_block(&ctx1).unwrap();

            let ctx_later = BlockRuntimeContext::new(
                block_ctx(
                    100,
                    GENESIS_TS_2024_01_01 + 30 * outbe_primitives::time::SECONDS_PER_DAY,
                ),
                ctx0.storage.clone(),
            );
            <RewardsModule as ProtocolModule>::begin_block(&ctx_later).unwrap();
        });
    }
}
