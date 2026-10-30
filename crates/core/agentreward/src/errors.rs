//! Error types for the AgentReward module.

use crate::precompile::IAgentReward;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AgentRewardError {
    #[error("agentreward tribute counter overflow")]
    TributeCounterOverflow,
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
    #[error("reward claim deadline {0} has passed")]
    ClaimDeadlinePassed(u64),
    #[error("reward balance has not expired at deadline {0}")]
    RewardBalanceNotExpired(u64),
    #[error("reward balance deadline overflow")]
    RewardDeadlineOverflow,
    #[error("nonempty reward balance has no deadline")]
    RewardDeadlineMissing,
    #[error("agentreward has no previous-day COEN price in {0}")]
    MissingPreviousDayPrice(u16),
    #[error("agentreward entry price overflow")]
    EntryPriceOverflow,
    #[error("agentreward unknown reward pool {0}")]
    UnknownRewardPool(u8),
    #[error("agentreward accrual job {0} is missing")]
    AccrualJobMissing(u64),
    #[error("date step kind {kind} from date key {date_key}")]
    DateStep { kind: u8, date_key: u32 },
}

pub(crate) fn date_step_error(err: outbe_primitives::time::TimeError) -> PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => AgentRewardError::DateStep { kind, date_key }.into(),
        None => PrecompileError::Fatal(err.to_string()),
    }
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<AgentRewardError> for PrecompileError {
    fn from(err: AgentRewardError) -> Self {
        use IAgentReward as I;
        match err {
            AgentRewardError::TributeCounterOverflow => revert_with(I::TributeCounterOverflow {}),
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
            AgentRewardError::ClaimDeadlinePassed(deadline) => {
                revert_with(I::ClaimDeadlinePassed { deadline })
            }
            AgentRewardError::RewardBalanceNotExpired(deadline) => {
                revert_with(I::RewardBalanceNotExpired { deadline })
            }
            AgentRewardError::RewardDeadlineOverflow => revert_with(I::RewardDeadlineOverflow {}),
            AgentRewardError::RewardDeadlineMissing => revert_with(I::RewardDeadlineMissing {}),
            AgentRewardError::MissingPreviousDayPrice(value) => {
                revert_with(I::MissingPreviousDayPrice { value })
            }
            AgentRewardError::EntryPriceOverflow => revert_with(I::EntryPriceOverflow {}),
            AgentRewardError::UnknownRewardPool(value) => {
                revert_with(I::UnknownRewardPool { value })
            }
            AgentRewardError::AccrualJobMissing(value) => {
                revert_with(I::AccrualJobMissing { value })
            }
            AgentRewardError::DateStep { kind, date_key } => revert_with(I::DateStep {
                kind,
                dateKey: date_key,
            }),
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
        for (error, expected) in [
            (
                AgentRewardError::ClaimDeadlinePassed(u64::MAX),
                IAgentReward::ClaimDeadlinePassed { deadline: u64::MAX }.abi_encode(),
            ),
            (
                AgentRewardError::RewardBalanceNotExpired(31_536_000),
                IAgentReward::RewardBalanceNotExpired {
                    deadline: 31_536_000,
                }
                .abi_encode(),
            ),
            (
                AgentRewardError::RewardDeadlineOverflow,
                IAgentReward::RewardDeadlineOverflow {}.abi_encode(),
            ),
            (
                AgentRewardError::RewardDeadlineMissing,
                IAgentReward::RewardDeadlineMissing {}.abi_encode(),
            ),
            (
                AgentRewardError::MissingPreviousDayPrice(840),
                IAgentReward::MissingPreviousDayPrice { value: 840 }.abi_encode(),
            ),
            (
                AgentRewardError::EntryPriceOverflow,
                IAgentReward::EntryPriceOverflow {}.abi_encode(),
            ),
            (
                AgentRewardError::TributeCounterOverflow,
                IAgentReward::TributeCounterOverflow {}.abi_encode(),
            ),
        ] {
            let err = PrecompileError::from(error);
            let PrecompileError::RevertBytes(bytes) = err else {
                panic!("expected RevertBytes, got {err:?}");
            };
            assert_eq!(bytes.as_ref(), expected.as_slice());
        }
    }

    #[test]
    fn entry_price_overflow_converts_to_canonical_abi_revert_bytes() {
        let err = PrecompileError::from(AgentRewardError::EntryPriceOverflow);
        let PrecompileError::RevertBytes(bytes) = err else {
            panic!("expected RevertBytes, got {err:?}");
        };
        assert_eq!(
            bytes.as_ref(),
            IAgentReward::EntryPriceOverflow {}.abi_encode()
        );
    }
}
