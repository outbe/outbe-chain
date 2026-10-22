use crate::precompile::IStaking;
use alloy_primitives::Bytes;
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolError;
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
    fn from(error: StakingError) -> Self {
        match error {
            StakingError::AmountZero => {
                PrecompileError::RevertBytes(Bytes::from(IStaking::AmountZero {}.abi_encode()))
            }
            StakingError::ThirdPartyStakingNotSupported => PrecompileError::RevertBytes(
                Bytes::from(IStaking::ThirdPartyStakingNotSupported {}.abi_encode()),
            ),
            StakingError::StakeExceedsMaxStakePercent => PrecompileError::RevertBytes(Bytes::from(
                IStaking::StakeExceedsMaxStakePercent {}.abi_encode(),
            )),
            StakingError::InsufficientStakedAmount => PrecompileError::RevertBytes(Bytes::from(
                IStaking::InsufficientStakedAmount {}.abi_encode(),
            )),
            StakingError::UnjailRequiresLiveEnclaveBinding => PrecompileError::RevertBytes(
                Bytes::from(IStaking::UnjailRequiresLiveEnclaveBinding {}.abi_encode()),
            ),
            StakingError::UnjailInsufficientStake { have, need } => PrecompileError::RevertBytes(
                Bytes::from(IStaking::UnjailInsufficientStake { have, need }.abi_encode()),
            ),
            StakingError::InvalidSlashPercent => PrecompileError::RevertBytes(Bytes::from(
                IStaking::InvalidSlashPercent {}.abi_encode(),
            )),
            StakingError::MsgValueMismatch => PrecompileError::RevertBytes(Bytes::from(
                IStaking::MsgValueMismatch {}.abi_encode(),
            )),
            StakingError::UnbondingCompletionTimestampOverflow => PrecompileError::RevertBytes(
                Bytes::from(IStaking::UnbondingCompletionTimestampOverflow {}.abi_encode()),
            ),
            StakingError::SlashedWithdrawalDelayOverflow => PrecompileError::RevertBytes(
                Bytes::from(IStaking::SlashedWithdrawalDelayOverflow {}.abi_encode()),
            ),
            error @ StakingError::StakeAccountingUnderflow { .. } => {
                PrecompileError::FatalWithCause(Box::new(error))
            }
            StakingError::MissingValidatorHistory => PrecompileError::RevertBytes(Bytes::from(
                IStaking::MissingValidatorHistory {}.abi_encode(),
            )),
            StakingError::ValidatorRegistryExceedsConsensusBound { count, bound } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IStaking::ValidatorRegistryExceedsConsensusBound { count, bound }.abi_encode(),
                ))
            }
            StakingError::SweepCountOverflow(value) => PrecompileError::RevertBytes(Bytes::from(
                IStaking::SweepCountOverflow {
                    value: value.to_string(),
                }
                .abi_encode(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(err: StakingError) -> Bytes {
        match PrecompileError::from(err) {
            PrecompileError::RevertBytes(bytes) => bytes,
            other => panic!("expected typed revert bytes, got {other:?}"),
        }
    }

    #[test]
    fn caller_revert_keeps_integer_and_string_fields() {
        let have = U256::from(1u64);
        let need = U256::from(2u64);
        let bytes = encoded(StakingError::UnjailInsufficientStake { have, need });
        let decoded = IStaking::UnjailInsufficientStake::abi_decode(&bytes).expect("stake payload");
        assert_eq!(decoded.have, have);
        assert_eq!(decoded.need, need);

        let bytes = encoded(StakingError::SweepCountOverflow("cursor"));
        let decoded = IStaking::SweepCountOverflow::abi_decode(&bytes).expect("string payload");
        assert_eq!(decoded.value, "cursor");
    }

    #[test]
    fn corrupt_accounting_is_fatal_with_typed_validator_cause() {
        let validator = Address::repeat_byte(0x11);
        let error = PrecompileError::from(StakingError::StakeAccountingUnderflow { validator });
        let cause = std::error::Error::source(&error)
            .and_then(|cause| cause.downcast_ref::<StakingError>());
        assert!(
            matches!(cause, Some(StakingError::StakeAccountingUnderflow { validator: actual }) if *actual == validator)
        );
        assert!(!error.is_node_local());
        assert!(error.into_precompile_result(0).is_err());
    }
}
