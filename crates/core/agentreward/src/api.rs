//! Cross-module API of AgentReward. Amounts are protocol units, not native COEN.
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;

use crate::schema::AgentRewardContract;
pub use crate::schema::RewardPool;

/// Sets the rate of `pool` for emission day `day`. The Validator and CCA rates come in the step
/// at 00:00 after the day, the WAA and SRA rates when the offering of the day ends.
pub fn set_agent_reward_rate(
    ctx: &BlockRuntimeContext<'_>,
    day: u32,
    pool: RewardPool,
) -> Result<()> {
    ctx.contract::<AgentRewardContract>()
        .set_agent_reward_rate(pool, day)
}
