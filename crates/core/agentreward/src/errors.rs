//! Error types for the AgentReward module.

use crate::precompile::IAgentReward;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AgentRewardError {
    #[error("agentreward day {0} is sealed or out of order")]
    DaySealed(u32),
    #[error("agentreward queue or job invariant violated")]
    InvalidJob,
    #[error("agentreward offer counter overflow")]
    OfferCounterOverflow,
    #[error("agentreward distribute_daily overflow")]
    DistributeDailyOverflow,
    #[error("reward distribution arithmetic overflow")]
    DistributionOverflow,
    #[error("agentreward total overflow")]
    AgentRewardTotalOverflow,
    #[error("agentreward pool total disagrees with its balances")]
    PoolTotalMismatch,
    #[error("agentreward pool reward overflow")]
    AgentRewardOverflow,
    #[error("no agent reward in this pool")]
    NoAgentReward,
    #[error("insufficient agent reward")]
    InsufficientAgentReward,
    #[error("agent reward deadline {0} has passed")]
    AgentRewardDeadlinePassed(u64),
    #[error("agent reward has not expired at deadline {0}")]
    AgentRewardNotExpired(u64),
    #[error("reward balance deadline overflow")]
    RewardDeadlineOverflow,
    #[error("nonempty reward balance has no deadline")]
    RewardDeadlineMissing,
    #[error("agentreward has no 24-hour COEN price in {0} at the redemption hour")]
    RedeemPriceUnavailable(u16),
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
            AgentRewardError::DaySealed(_) | AgentRewardError::InvalidJob => {
                PrecompileError::Fatal(err.to_string())
            }
            AgentRewardError::OfferCounterOverflow => revert_with(I::OfferCounterOverflow {}),
            AgentRewardError::DistributeDailyOverflow => revert_with(I::DistributeDailyOverflow {}),
            AgentRewardError::DistributionOverflow => revert_with(I::DistributionOverflow {}),
            AgentRewardError::AgentRewardTotalOverflow => {
                revert_with(I::AgentRewardTotalOverflow {})
            }
            AgentRewardError::PoolTotalMismatch => revert_with(I::PoolTotalMismatch {}),
            AgentRewardError::AgentRewardOverflow => revert_with(I::AgentRewardOverflow {}),
            AgentRewardError::NoAgentReward => revert_with(I::NoAgentReward {}),
            AgentRewardError::InsufficientAgentReward => revert_with(I::InsufficientAgentReward {}),
            AgentRewardError::AgentRewardDeadlinePassed(deadline) => {
                revert_with(I::AgentRewardDeadlinePassed { deadline })
            }
            AgentRewardError::AgentRewardNotExpired(deadline) => {
                revert_with(I::AgentRewardNotExpired { deadline })
            }
            AgentRewardError::RewardDeadlineOverflow => revert_with(I::RewardDeadlineOverflow {}),
            AgentRewardError::RewardDeadlineMissing => revert_with(I::RewardDeadlineMissing {}),
            AgentRewardError::RedeemPriceUnavailable(iso_code) => {
                revert_with(I::RedeemPriceUnavailable { isoCode: iso_code })
            }
            AgentRewardError::EntryPriceOverflow => revert_with(I::EntryPriceOverflow {}),
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
        for (error, expected) in [
            (
                AgentRewardError::AgentRewardDeadlinePassed(u64::MAX),
                IAgentReward::AgentRewardDeadlinePassed { deadline: u64::MAX }.abi_encode(),
            ),
            (
                AgentRewardError::AgentRewardNotExpired(31_536_000),
                IAgentReward::AgentRewardNotExpired {
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
                AgentRewardError::RedeemPriceUnavailable(840),
                IAgentReward::RedeemPriceUnavailable { isoCode: 840 }.abi_encode(),
            ),
            (
                AgentRewardError::EntryPriceOverflow,
                IAgentReward::EntryPriceOverflow {}.abi_encode(),
            ),
            (
                AgentRewardError::OfferCounterOverflow,
                IAgentReward::OfferCounterOverflow {}.abi_encode(),
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
