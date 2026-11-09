//! Genesis validator-set and staking cross-check.

use alloy_primitives::U256;
use outbe_primitives::block::BlockHeightRegime;
use outbe_primitives::consensus::{GenesisValidator, GenesisValidators};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_validatorset::{contract::ValidatorSet, ValidatorLifecycle, ValidatorState};

use crate::contract::Staking;
use crate::errors::GenesisValidationError;

/// Validates genesis validator-set and staking state outside steady state.
pub fn validate_genesis_state_for_regime(
    regime: BlockHeightRegime,
    storage: StorageHandle<'_>,
    genesis_validators: Option<&GenesisValidators>,
) -> Result<()> {
    if regime.is_steady_state() {
        return Ok(());
    }
    if let Some(genesis) = genesis_validators {
        validate_genesis_state(storage, genesis)?;
    }
    Ok(())
}

/// Verifies that canonical genesis storage matches `genesis`.
pub fn validate_genesis_state(
    storage: StorageHandle<'_>,
    genesis: &GenesisValidators,
) -> Result<()> {
    let validator_set = ValidatorSet::new(storage.clone());
    check_genesis_configuration(&validator_set, genesis)?;

    let staking = Staking::new(storage);
    let min_stake = staking.min_stake()?;
    let mut expected_total = U256::ZERO;
    for validator in &genesis.validators {
        let state = validator_set.validator_state(validator.address)?;
        check_genesis_member(&state, validator, min_stake)?;
        let staking_amount = staking.stake_amount.read(&validator.address)?;
        expected_total = expected_total
            .checked_add(staking_amount)
            .ok_or(GenesisValidationError::GenesisTotalStakeOverflow)?;
    }

    let total_staked = staking.total_staked.read()?;
    if total_staked != expected_total {
        return Err(GenesisValidationError::GenesisTotalStakedMismatch {
            state: total_staked,
            expected: expected_total,
        }
        .into());
    }
    Ok(())
}

fn check_genesis_configuration(
    validator_set: &ValidatorSet<'_>,
    genesis: &GenesisValidators,
) -> Result<()> {
    if !validator_set.config_is_initialized.read()? {
        return Err(GenesisValidationError::ValidatorSetUninitializedInGenesis.into());
    }
    let epoch_length_blocks = validator_set.config_epoch_length_blocks.read()?;
    if epoch_length_blocks != genesis.epoch_length_blocks {
        return Err(GenesisValidationError::GenesisEpochLengthMismatch {
            state: epoch_length_blocks,
            genesis: genesis.epoch_length_blocks,
        }
        .into());
    }
    let active_consensus_count = validator_set.active_consensus_count()?;
    if active_consensus_count as usize != genesis.validators.len() {
        return Err(
            GenesisValidationError::GenesisActiveConsensusCountMismatch {
                state: active_consensus_count,
                genesis: genesis.validators.len(),
            }
            .into(),
        );
    }
    Ok(())
}

fn check_genesis_member(
    state: &ValidatorState,
    validator: &GenesisValidator,
    min_stake: U256,
) -> Result<()> {
    if !state.is_registered() {
        return Err(GenesisValidationError::MissingGenesisValidator {
            address: validator.address,
        }
        .into());
    }
    if state.consensus_pubkey().copied() != Some(validator.consensus_pubkey) {
        return Err(GenesisValidationError::GenesisConsensusPubkeyMismatch {
            address: validator.address,
        }
        .into());
    }
    if !matches!(state.lifecycle(), ValidatorLifecycle::Active(_)) {
        return Err(GenesisValidationError::GenesisValidatorNotActive {
            address: validator.address,
        }
        .into());
    }
    if state.bonded_stake() < min_stake {
        return Err(GenesisValidationError::GenesisValidatorStakeBelowMinimum {
            address: validator.address,
        }
        .into());
    }
    Ok(())
}
