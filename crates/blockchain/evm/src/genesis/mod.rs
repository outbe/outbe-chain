//! Block 0 (`Genesis`) and initial genesis-state verification.
//!
//! Isolates all Block-0 (`BlockHeightRegime::Genesis`) execution invariants and
//! canonical genesis validator-set state verification behind a single module boundary.

use alloy_evm::block::BlockExecutionError;
use alloy_primitives::Address;
use outbe_primitives::{
    addresses::REWARDS_ADDRESS, block::BlockHeightRegime, consensus::GenesisValidators,
    error::Result as OutbeResult, reshare_artifact::OutbeBlockArtifacts, storage::StorageHandle,
};
use outbe_tee::witness::BlockWitnessFault;

use crate::{errors::ExecutorValidationError, executor::witness::witness_fault_to_block_error};

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
    crate::active_evm_runtime_host().validate_genesis_state_for_regime(
        regime,
        storage,
        genesis_validators,
    )
}

/// Verifies that the canonical genesis storage matches the configured [`GenesisValidators`].
pub fn validate_genesis_state(
    storage: StorageHandle,
    genesis: &GenesisValidators,
) -> OutbeResult<()> {
    crate::active_evm_runtime_host().validate_genesis_state(storage, genesis)
}
