use alloy_primitives::{Address, U256};
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Staking module errors.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StakingError {
    #[error("amount must be non-zero")]
    AmountZero,
    #[error("third-party staking not supported: caller must be validator")]
    ThirdPartyStakingNotSupported,
    #[error("stake would exceed max_stake_percent")]
    StakeExceedsMaxStakePercent,
    #[error("insufficient staked amount")]
    InsufficientStakedAmount,
    #[error("unjailValidator requires a live admitted enclave binding")]
    UnjailRequiresLiveEnclaveBinding,
    #[error("unjailValidator requires stake >= min_stake: have {have}, need {need}")]
    UnjailInsufficientStake { have: U256, need: U256 },
    #[error("slash percent must be <= 100")]
    InvalidSlashPercent,
    #[error("msg.value must equal stake amount")]
    MsgValueMismatch,
    #[error("unbonding completion timestamp overflow")]
    UnbondingCompletionTimestampOverflow,
    #[error("slashed withdrawal delay overflow")]
    SlashedWithdrawalDelayOverflow,
    #[error("stake accounting underflow for validator {validator}")]
    StakeAccountingUnderflow { validator: Address },
    #[error("registered validator is missing history")]
    MissingValidatorHistory,
    #[error("validator registry exceeds consensus bound: {count} > {bound}")]
    ValidatorRegistryExceedsConsensusBound { count: u32, bound: u32 },
    #[error("{0}")]
    SweepCountOverflow(&'static str),
}

impl From<StakingError> for PrecompileError {
    fn from(err: StakingError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
