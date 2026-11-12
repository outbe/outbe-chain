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
    #[error("stake accounting overflow for validator {validator}")]
    StakeAccountingOverflow { validator: Address },
    #[error("total staked accounting overflow")]
    TotalStakeAccountingOverflow,
    #[error("slash amount overflow")]
    SlashAmountOverflow,
    #[error("registered validator is missing history")]
    MissingValidatorHistory,
    #[error("validator registry exceeds consensus bound: {count} > {bound}")]
    ValidatorRegistryExceedsConsensusBound { count: u32, bound: u32 },
    #[error("{0}")]
    SweepCountOverflow(&'static str),
    #[error("validator {validator} reached the unbonding entry limit ({max_entries})")]
    TooManyUnbondingEntries {
        validator: Address,
        max_entries: u32,
    },
    #[error("unbonding queue index overflow")]
    UnbondingIndexOverflow,
    #[error("claimable unbonding amount overflow")]
    ClaimableAmountOverflow,
    #[error("unbonding queue length underflow for validator {validator}")]
    UnbondingLengthUnderflow { validator: Address },
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
            error @ (StakingError::StakeAccountingUnderflow { .. }
            | StakingError::UnbondingLengthUnderflow { .. }) => {
                PrecompileError::FatalWithCause(Box::new(error))
            }
            StakingError::StakeAccountingOverflow { validator } => PrecompileError::RevertBytes(
                Bytes::from(IStaking::StakeAccountingOverflow { validator }.abi_encode()),
            ),
            StakingError::TotalStakeAccountingOverflow => PrecompileError::RevertBytes(
                Bytes::from(IStaking::TotalStakeAccountingOverflow {}.abi_encode()),
            ),
            StakingError::SlashAmountOverflow => PrecompileError::RevertBytes(Bytes::from(
                IStaking::SlashAmountOverflow {}.abi_encode(),
            )),
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
            StakingError::TooManyUnbondingEntries {
                validator,
                max_entries,
            } => PrecompileError::RevertBytes(Bytes::from(
                IStaking::TooManyUnbondingEntries {
                    validator,
                    maxEntries: max_entries,
                }
                .abi_encode(),
            )),
            StakingError::UnbondingIndexOverflow => PrecompileError::RevertBytes(Bytes::from(
                IStaking::UnbondingIndexOverflow {}.abi_encode(),
            )),
            StakingError::ClaimableAmountOverflow => PrecompileError::RevertBytes(Bytes::from(
                IStaking::ClaimableAmountOverflow {}.abi_encode(),
            )),
        }
    }
}

/// Failures of the genesis staking and validator-set cross-check.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GenesisValidationError {
    /// Sum of member stakes overflowed.
    #[error("genesis total stake overflow")]
    GenesisTotalStakeOverflow,
    /// Stored `total_staked` does not equal the sum of member stakes.
    #[error("genesis total_staked mismatch: state={state}, expected={expected}")]
    GenesisTotalStakedMismatch {
        /// Stored total.
        state: U256,
        /// Sum of member stakes.
        expected: U256,
    },
    /// `min_stake` was not written at genesis.
    #[error("Staking min_stake must be initialized in genesis")]
    StakingMinStakeUninitialized,
    /// Validator-set config slot is empty.
    #[error("ValidatorSet must be initialized in genesis; executor genesis backfill is disabled")]
    ValidatorSetUninitializedInGenesis,
    /// Stored epoch length does not match the genesis document.
    #[error("genesis ValidatorSet epoch_length_blocks mismatch: state={state}, genesis={genesis}")]
    GenesisEpochLengthMismatch {
        /// Stored epoch length.
        state: u32,
        /// Genesis document epoch length.
        genesis: u32,
    },
    /// Active consensus count does not match the genesis validator list.
    #[error(
        "genesis active consensus set size mismatch: state={state}, genesis validators={genesis}"
    )]
    GenesisActiveConsensusCountMismatch {
        /// Stored active consensus count.
        state: u32,
        /// Genesis validator count.
        genesis: usize,
    },
    /// A genesis validator has no validator-set record.
    #[error("genesis validator {address} is missing from ValidatorSet")]
    MissingGenesisValidator {
        /// Missing validator.
        address: Address,
    },
    /// Stored consensus key does not match the genesis document.
    #[error("genesis validator {address} consensus pubkey mismatch")]
    GenesisConsensusPubkeyMismatch {
        /// Validator whose key differs.
        address: Address,
    },
    /// Genesis validator is not in the active lifecycle.
    #[error("genesis validator {address} must be active with a BLS share")]
    GenesisValidatorNotActive {
        /// Validator that is not active.
        address: Address,
    },
    /// Bonded stake is below `min_stake`.
    #[error("genesis validator {address} stake below min_stake")]
    GenesisValidatorStakeBelowMinimum {
        /// Validator below the minimum.
        address: Address,
    },
}

impl From<GenesisValidationError> for PrecompileError {
    fn from(error: GenesisValidationError) -> Self {
        PrecompileError::Fatal(error.to_string())
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
