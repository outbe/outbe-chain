//! Error types for the AgentReward module.

use crate::precompile::IAgentReward;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AgentRewardError {
    #[error("agentreward rate of pool {pool} and day {day} is not due")]
    RateNotDue { pool: u8, day: u32 },
    #[error("agentreward rate arithmetic overflow")]
    RateOverflow,
    #[error("agentreward claims of pool {pool} and day {day} exceed its allocated amount")]
    UnclaimedUnderflow { pool: u8, day: u32 },
    #[error("agentreward offer counter overflow")]
    OfferCounterOverflow,
    #[error("agentreward rate of pool {pool} and day {day} is already set")]
    AgentRewardRateAlreadySet { pool: u8, day: u32 },
    #[error("agentreward pool {pool} has no rate for day {day}")]
    AgentRewardRateMissing { pool: u8, day: u32 },
    #[error("agentreward claim names {0} days")]
    InvalidRewardDays(usize),
    #[error("no agent reward weight in day {0}")]
    NoAgentRewardWeight(u32),
    #[error("no agent reward in these days")]
    NoAgentReward,
    #[error("agent reward deadline {0} has passed")]
    AgentRewardDeadlinePassed(u64),
    #[error("agent reward has not expired at deadline {0}")]
    AgentRewardNotExpired(u64),
    #[error("reward deadline overflow")]
    RewardDeadlineOverflow,
    #[error("agentreward has no 24-hour COEN price in {0} at the redemption hour")]
    RedeemPriceUnavailable(u16),
    #[error("agentreward entry price overflow")]
    EntryPriceOverflow,
    #[error("agentreward unknown reward pool {0}")]
    UnknownRewardPool(u8),
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<AgentRewardError> for PrecompileError {
    fn from(err: AgentRewardError) -> Self {
        use IAgentReward as I;
        match err {
            AgentRewardError::RateNotDue { .. } => PrecompileError::FatalWithCause(Box::new(err)),
            AgentRewardError::RateOverflow | AgentRewardError::UnclaimedUnderflow { .. } => {
                PrecompileError::Fatal(err.to_string())
            }
            AgentRewardError::OfferCounterOverflow => revert_with(I::OfferCounterOverflow {}),
            AgentRewardError::AgentRewardRateAlreadySet { pool, day } => {
                revert_with(I::AgentRewardRateAlreadySet {
                    pool,
                    emissionDay: day,
                })
            }
            AgentRewardError::AgentRewardRateMissing { pool, day } => {
                revert_with(I::AgentRewardRateMissing {
                    pool,
                    emissionDay: day,
                })
            }
            AgentRewardError::InvalidRewardDays(count) => revert_with(I::InvalidRewardDays {
                count: alloy_primitives::U256::from(count),
            }),
            AgentRewardError::NoAgentRewardWeight(day) => {
                revert_with(I::NoAgentRewardWeight { emissionDay: day })
            }
            AgentRewardError::NoAgentReward => revert_with(I::NoAgentReward {}),
            AgentRewardError::AgentRewardDeadlinePassed(deadline) => {
                revert_with(I::AgentRewardDeadlinePassed { deadline })
            }
            AgentRewardError::AgentRewardNotExpired(deadline) => {
                revert_with(I::AgentRewardNotExpired { deadline })
            }
            AgentRewardError::RewardDeadlineOverflow => revert_with(I::RewardDeadlineOverflow {}),
            AgentRewardError::RedeemPriceUnavailable(iso_code) => {
                revert_with(I::RedeemPriceUnavailable { isoCode: iso_code })
            }
            AgentRewardError::EntryPriceOverflow => revert_with(I::EntryPriceOverflow {}),
            AgentRewardError::UnknownRewardPool(value) => {
                revert_with(I::UnknownRewardPool { value })
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
    fn user_errors_convert_to_their_abi_revert_bytes() {
        for (error, expected) in [
            (
                AgentRewardError::AgentRewardRateMissing {
                    pool: 1,
                    day: 20_231_113,
                },
                IAgentReward::AgentRewardRateMissing {
                    pool: 1,
                    emissionDay: 20_231_113,
                }
                .abi_encode(),
            ),
            (
                AgentRewardError::InvalidRewardDays(65),
                IAgentReward::InvalidRewardDays {
                    count: alloy_primitives::U256::from(65),
                }
                .abi_encode(),
            ),
            (
                AgentRewardError::NoAgentRewardWeight(20_231_113),
                IAgentReward::NoAgentRewardWeight {
                    emissionDay: 20_231_113,
                }
                .abi_encode(),
            ),
            (
                AgentRewardError::NoAgentReward,
                IAgentReward::NoAgentReward {}.abi_encode(),
            ),
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
            (
                AgentRewardError::UnknownRewardPool(4),
                IAgentReward::UnknownRewardPool { value: 4 }.abi_encode(),
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
    fn chain_errors_do_not_revert() {
        for error in [
            AgentRewardError::RateNotDue {
                pool: 0,
                day: 20_231_113,
            },
            AgentRewardError::RateOverflow,
            AgentRewardError::UnclaimedUnderflow {
                pool: 0,
                day: 20_231_113,
            },
        ] {
            assert!(!matches!(
                PrecompileError::from(error),
                PrecompileError::Revert(_) | PrecompileError::RevertBytes(_)
            ));
        }
    }
}
