//! Epoch-counter reset that runs before late-finalize credits on a boundary block.

use alloy_primitives::Address;
use outbe_primitives::consensus::DkgBoundaryArtifact;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_validatorset::contract::ValidatorSet;

use crate::schema::SlashIndicator;

/// Resets outgoing-epoch counters when this block advances the certified epoch.
pub fn prepare_boundary_epoch_counters(
    storage: StorageHandle<'_>,
    boundary: &DkgBoundaryArtifact,
    block_number: u64,
) -> Result<()> {
    let validators = ValidatorSet::new(storage.clone());
    let current_epoch = validators.current_epoch_u64()?;
    let advances = outbe_validatorset::validate_boundary_epoch_transition(
        current_epoch,
        boundary.epoch,
        block_number,
    )?;
    if !advances {
        return Ok(());
    }
    let addresses = validator_addresses(&validators)?;
    SlashIndicator::new(storage.clone()).reset_epoch_counters(&addresses)?;
    outbe_validatorset::hooks::reset_epoch_counters(storage)
}

fn validator_addresses(validators: &ValidatorSet<'_>) -> Result<Vec<Address>> {
    Ok(validators
        .get_all_validators()?
        .into_iter()
        .map(|validator| validator.validator_address)
        .collect())
}
