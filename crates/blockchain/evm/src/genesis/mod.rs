//! Block 0 (`Genesis`) and initial genesis-state verification.
//!
//! Isolates all Block-0 (`BlockHeightRegime::Genesis`) execution invariants and
//! canonical genesis validator-set state verification behind a single module boundary.

use alloy_evm::block::BlockExecutionError;
use alloy_primitives::{Address, U256};
use outbe_primitives::{
    addresses::REWARDS_ADDRESS,
    block::BlockHeightRegime,
    consensus::{GenesisValidator, GenesisValidators},
    error::Result as OutbeResult,
    reshare_artifact::OutbeBlockArtifacts,
    storage::StorageHandle,
};
use outbe_tee::witness::BlockWitnessFault;
use outbe_validatorset::{contract::ValidatorSet, ValidatorLifecycle, ValidatorState};

use crate::{
    errors::{ExecutorHookError, ExecutorValidationError},
    executor::witness::witness_fault_to_block_error,
};

/// Validates the block header's `beneficiary` under the block's height regime.
///
/// Block 0 (`Genesis`) allows any beneficiary. All subsequent regimes
/// (`GenesisBootstrap` and `SteadyState`) require [`REWARDS_ADDRESS`].
pub fn validate_block_beneficiary(
    regime: BlockHeightRegime,
    beneficiary: Address,
) -> Result<(), ExecutorValidationError> {
    if regime.requires_rewards_beneficiary() && beneficiary != REWARDS_ADDRESS {
        return Err(ExecutorValidationError::InvalidNonGenesisBeneficiary {
            expected: REWARDS_ADDRESS,
            actual: beneficiary,
        });
    }
    Ok(())
}

/// Rejects `enclave_witness` on Block 0 (`Genesis`).
pub fn validate_genesis_enclave_witness(
    regime: BlockHeightRegime,
    block_artifacts: &OutbeBlockArtifacts,
) -> Result<(), BlockExecutionError> {
    if regime.is_genesis() && block_artifacts.enclave_witness.is_some() {
        return Err(witness_fault_to_block_error(
            BlockWitnessFault::TapeInvalid,
            "genesis block 0 must not carry enclave_witness",
        ));
    }
    Ok(())
}

/// Validates genesis validator-set and staking state when executing in a
/// non-steady-state regime (`Genesis` or `GenesisBootstrap`).
pub fn validate_genesis_state_for_regime(
    regime: BlockHeightRegime,
    storage: StorageHandle,
    genesis_validators: Option<&GenesisValidators>,
) -> OutbeResult<()> {
    if !regime.is_steady_state() {
        if let Some(genesis) = genesis_validators {
            validate_genesis_state(storage, genesis)?;
        }
    }
    Ok(())
}

/// Verifies that the canonical genesis storage matches the configured [`GenesisValidators`].
pub fn validate_genesis_state(
    storage: StorageHandle,
    genesis: &GenesisValidators,
) -> OutbeResult<()> {
    let vs = ValidatorSet::new(storage.clone());
    validate_genesis_configuration(&vs, genesis)?;

    let staking = outbe_staking::contract::Staking::new(storage);
    let min_stake = staking.min_stake()?;

    let mut expected_total = U256::ZERO;
    for validator in &genesis.validators {
        let state = vs.validator_state(validator.address)?;
        validate_genesis_member(&state, validator, min_stake)?;

        let staking_amount = staking.stake_amount.read(&validator.address)?;
        expected_total = expected_total
            .checked_add(staking_amount)
            .ok_or(ExecutorHookError::GenesisTotalStakeOverflow)?;
    }

    let total_staked = staking.total_staked.read()?;
    if total_staked != expected_total {
        return Err(ExecutorHookError::GenesisTotalStakedMismatch {
            state: total_staked,
            expected: expected_total,
        }
        .into());
    }

    Ok(())
}

fn validate_genesis_configuration(
    vs: &ValidatorSet<'_>,
    genesis: &GenesisValidators,
) -> OutbeResult<()> {
    if !vs.config_is_initialized.read()? {
        return Err(ExecutorHookError::ValidatorSetUninitializedInGenesis.into());
    }

    let epoch_length_blocks = vs.config_epoch_length_blocks.read()?;
    if epoch_length_blocks != genesis.epoch_length_blocks {
        return Err(ExecutorHookError::GenesisEpochLengthMismatch {
            state: epoch_length_blocks,
            genesis: genesis.epoch_length_blocks,
        }
        .into());
    }

    let active_consensus_count = vs.active_consensus_count()?;
    if active_consensus_count as usize != genesis.validators.len() {
        return Err(ExecutorHookError::GenesisActiveConsensusCountMismatch {
            state: active_consensus_count,
            genesis: genesis.validators.len(),
        }
        .into());
    }

    Ok(())
}

fn validate_genesis_member(
    state: &ValidatorState,
    validator: &GenesisValidator,
    min_stake: U256,
) -> OutbeResult<U256> {
    if !state.is_registered() {
        return Err(ExecutorHookError::MissingGenesisValidator {
            address: validator.address,
        }
        .into());
    }

    if state.consensus_pubkey().copied() != Some(validator.consensus_pubkey) {
        return Err(ExecutorHookError::GenesisConsensusPubkeyMismatch {
            address: validator.address,
        }
        .into());
    }
    if !matches!(state.lifecycle(), ValidatorLifecycle::Active(_)) {
        return Err(ExecutorHookError::GenesisValidatorNotActive {
            address: validator.address,
        }
        .into());
    }
    let bonded_stake = state.bonded_stake();
    if bonded_stake < min_stake {
        return Err(ExecutorHookError::GenesisValidatorStakeBelowMinimum {
            address: validator.address,
        }
        .into());
    }

    Ok(bonded_stake)
}
