//! Error types for the AgentReward module.

use crate::precompile::IAgentReward;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
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
    #[error("agentreward entry price overflow")]
    EntryPriceOverflow,
    #[error("agentreward unknown reward pool {0}")]
    UnknownRewardPool(u8),
    #[error("agentreward accrual job {0} is missing")]
    AccrualJobMissing(u64),
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<AgentRewardError> for PrecompileError {
    fn from(err: AgentRewardError) -> Self {
        use IAgentReward as I;
        match err {
            AgentRewardError::DistributeDailyOverflow => revert_with(I::DistributeDailyOverflow {}),
            AgentRewardError::DistributionOverflow => revert_with(I::DistributionOverflow {}),
            AgentRewardError::ClaimableTotalOverflow => revert_with(I::ClaimableTotalOverflow {}),
            AgentRewardError::PoolTotalMismatch => revert_with(I::PoolTotalMismatch {}),
            AgentRewardError::ClaimableRewardsOverflow => {
                revert_with(I::ClaimableRewardsOverflow {})
            }
            AgentRewardError::NoClaimableBalance => revert_with(I::NoClaimableBalance {}),
            AgentRewardError::InsufficientClaimableBalance => {
                revert_with(I::InsufficientClaimableBalance {})
            }
            AgentRewardError::MissingPreviousDayPrice(value) => {
                revert_with(I::MissingPreviousDayPrice { value })
            }
            AgentRewardError::UnknownRewardPool(value) => {
                revert_with(I::UnknownRewardPool { value })
            }
            AgentRewardError::AccrualJobMissing(value) => {
                revert_with(I::AccrualJobMissing { value })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AgentRewardError;
    use crate::precompile::IAgentReward;
    use alloy_sol_types::SolError;
    use outbe_primitives::error::PrecompileError;

    #[test]
    fn agent_reward_error_converts_to_canonical_abi_revert_bytes() {
        let err = PrecompileError::from(AgentRewardError::MissingPreviousDayPrice(840));
        let PrecompileError::RevertBytes(bytes) = err else {
            panic!("expected RevertBytes, got {err:?}");
        };
        assert_eq!(
            bytes.as_ref(),
            IAgentReward::MissingPreviousDayPrice { value: 840 }
                .abi_encode()
                .as_slice()
        );
    }
}
