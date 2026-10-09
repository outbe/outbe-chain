//! Block lifecycle hook for the Rewards module.
//!
//! `RewardsLifecycle` is the zero-sized marker type that implements
//! [`BlockLifecycle`] and is registered in the executor's pre-execution
//! ordering. It runs at the start of every block and currently performs
//! only the genesis-anchor lazy initialization.
//!
//! Day-boundary settle moved out of Rewards as part of the Cycle
//! refactor. The daily orchestration runs on
//! `CycleLifecycle::begin_block` and dispatches into EmissionLimit ->
//! AgentReward -> Rewards (via
//! [`crate::api::prepare_daily_validator_gem_batch`]) exactly once per UTC day.

use outbe_primitives::{block::BlockLifecycle, block::BlockRuntimeContext, error::Result};

use crate::runtime;

/// Zero-sized marker implementing the block-lifecycle contract for the
/// Rewards module. Registered in
/// `outbe_evm::executor::run_outbe_pre_execution_hooks` so the executor
/// can keep ordering explicit and hard-fork governed
pub struct RewardsLifecycle;

impl BlockLifecycle for RewardsLifecycle {
    type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        // Record `genesis_utc_day` from block 0's timestamp on the very
        // first invocation of this lifecycle on a fresh chain.
        // Subsequent calls are no-ops because the slot is already
        // non-zero. `day_emission_limit` in `outbe_emissionlimit::day_emission`
        // reads this anchor through `day_number_since_genesis`.
        let _genesis = runtime::ensure_genesis_anchor(ctx)?;
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{block_ctx, with_block, GENESIS_TS as GENESIS_TS_2024_01_01};

    #[test]
    fn begin_block_locks_in_genesis_utc_day_on_block_zero() {
        with_block(0, GENESIS_TS_2024_01_01, |ctx| {
            <RewardsLifecycle as BlockLifecycle>::begin_block(&ctx).unwrap();

            assert_eq!(runtime::genesis_utc_day(&ctx).unwrap(), 20240101);
        });
    }

    #[test]
    fn begin_block_is_idempotent_across_blocks() {
        with_block(0, GENESIS_TS_2024_01_01, |ctx0| {
            <RewardsLifecycle as BlockLifecycle>::begin_block(&ctx0).unwrap();

            // Block 1, slightly later - must not move the locked-in day.
            let ctx1 = BlockRuntimeContext::new(
                block_ctx(1, GENESIS_TS_2024_01_01 + 60),
                ctx0.storage.clone(),
            );
            <RewardsLifecycle as BlockLifecycle>::begin_block(&ctx1).unwrap();
            assert_eq!(runtime::genesis_utc_day(&ctx1).unwrap(), 20240101);

            // Block 100, 30 days later - still 20240101.
            let ctx_later = BlockRuntimeContext::new(
                block_ctx(100, GENESIS_TS_2024_01_01 + 86_400 * 30),
                ctx0.storage.clone(),
            );
            <RewardsLifecycle as BlockLifecycle>::begin_block(&ctx_later).unwrap();
            assert_eq!(runtime::genesis_utc_day(&ctx_later).unwrap(), 20240101);
        });
    }
}
