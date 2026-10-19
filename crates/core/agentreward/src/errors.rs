//! Error types for the AgentReward module.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AgentRewardError {
    #[error("agentreward distribute_daily overflow")]
    DistributeDailyOverflow,
    #[error("reward distribution arithmetic overflow")]
    DistributionOverflow,
    #[error("agentreward claimable total overflow")]
    ClaimableTotalOverflow,
    #[error("agentreward pool total disagrees with its balances")]
    PoolTotalMismatch,
    #[error("agentreward claimable_rewards overflow")]
    ClaimableRewardsOverflow,
    #[error("no claimable balance in this pool")]
    NoClaimableBalance,
    #[error("insufficient claimable balance")]
    InsufficientClaimableBalance,
    #[error("agentreward has no previous-day COEN price in {0}")]
    MissingPreviousDayPrice(u16),
    #[error("reward timestamp overflow")]
    TimestampOverflow,
    #[error("agentreward unknown reward pool {0}")]
    UnknownRewardPool(u8),
    #[error("agentreward accrual job {0} is missing")]
    AccrualJobMissing(u64),
}

impl From<AgentRewardError> for PrecompileError {
    fn from(err: AgentRewardError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
