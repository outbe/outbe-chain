//! Error types for the AgentReward module.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("reward distribution arithmetic overflow")]
pub struct DistributionError;

impl From<DistributionError> for PrecompileError {
    fn from(err: DistributionError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AgentRewardError {
    #[error("agentreward distribute_daily overflow")]
    DistributeDailyOverflow,
    #[error("native AgentReward share overflow")]
    NativeShareOverflow,
    #[error(transparent)]
    Distribution(#[from] DistributionError),
    #[error("agentreward claimable total overflow")]
    ClaimableTotalOverflow,
    #[error("agentreward claimable_rewards overflow")]
    ClaimableRewardsOverflow,
    #[error("no claimable balance in this pool")]
    NoClaimableBalance,
    #[error("insufficient claimable balance")]
    InsufficientClaimableBalance,
    #[error("claimed amount is below one protocol unit")]
    ClaimBelowProtocolUnit,
    #[error("native AgentReward claim overflow")]
    NativeClaimOverflow,
    #[error("agentreward has no previous-day COEN price in {0}")]
    MissingPreviousDayPrice(u16),
    #[error("reward timestamp overflow")]
    TimestampOverflow,
    #[error("agentreward unknown reward pool {0}")]
    UnknownRewardPool(u8),
}

impl From<AgentRewardError> for PrecompileError {
    fn from(err: AgentRewardError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AgentRewardFatalError {
    #[error("claimReward requires the compressed-entity readers route")]
    ClaimRewardRequiresReaders,
}

impl From<AgentRewardFatalError> for PrecompileError {
    fn from(err: AgentRewardFatalError) -> Self {
        PrecompileError::Fatal(err.to_string())
    }
}
