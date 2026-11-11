//! Certified DKG boundary transition against validator-set storage.

use alloy_primitives::{keccak256, Address, B256};
use outbe_primitives::block::BlockHeightRegime;
use outbe_primitives::consensus::DkgBoundaryArtifact;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::contract::ValidatorSet;
use crate::errors::BoundaryError;
use crate::{hooks, CommitteeEntry, CommitteeSnapshot};

/// Applies a certified boundary and writes the activated committee snapshot.
pub fn apply_boundary_outcome(
    storage: StorageHandle<'_>,
    boundary: &DkgBoundaryArtifact,
    block_number: u64,
    timestamp: u64,
) -> Result<()> {
    let reshare = &boundary.reshare;
    check_boundary_commitments(boundary, reshare.active_set_hash, &reshare.new_active_set)?;
    let incoming_snapshot = committee_snapshot_from_boundary(storage.clone(), boundary)?;
    check_committee_set_hash(boundary, &incoming_snapshot)?;

    let validator_set = ValidatorSet::new(storage.clone());
    let current_hash = validator_set.active_consensus_set_hash()?;
    let current_epoch = epoch_as_u64(validator_set.epoch_snapshot()?.number)?;
    if current_hash != reshare.active_set_hash && !boundary.is_validator_set_change {
        return Err(
            BoundaryError::ActiveSetHashChangedWithoutValidatorSetChange {
                current: current_hash,
                boundary: reshare.active_set_hash,
            }
            .into(),
        );
    }
    let advances_epoch =
        validate_boundary_epoch_transition(current_epoch, boundary.epoch, block_number)?;

    let activation_guard = storage.checkpoint_guard()?;
    if advances_epoch {
        hooks::advance_epoch(storage.clone(), timestamp, block_number)?;
    }
    let inputs = hooks::BoundaryActivationInputs {
        outgoing: None,
        incoming_epoch: boundary.epoch,
        incoming: incoming_snapshot,
        freeze_height: boundary.freeze_height,
        new_active_set: reshare.new_active_set.clone(),
        active_set_hash: reshare.active_set_hash,
        tee_expired_target_exclusions: boundary.tee_expired_target_exclusions.clone(),
    };
    hooks::activate_boundary_atomic(storage.clone(), &inputs)?;
    if advances_epoch {
        ValidatorSet::new(storage).cleanup_inactive_validators(16)?;
    }
    activation_guard.commit()?;
    Ok(())
}

/// Builds the committee snapshot named by `boundary`.
pub fn committee_snapshot_from_boundary(
    storage: StorageHandle<'_>,
    boundary: &DkgBoundaryArtifact,
) -> Result<CommitteeSnapshot> {
    let validator_set = ValidatorSet::new(storage);
    let mut committee = Vec::with_capacity(boundary.reshare.new_active_set.len());
    for address in &boundary.reshare.new_active_set {
        let Some(consensus_pubkey) = validator_set.consensus_pubkey_of(*address)? else {
            return Err(BoundaryError::UnregisteredBoundaryValidator { address: *address }.into());
        };
        committee.push(CommitteeEntry {
            address: *address,
            consensus_pubkey,
        });
    }
    Ok(CommitteeSnapshot {
        committee,
        vrf_material_version: boundary.vrf_material_version,
        vrf_group_public_key_bytes: boundary.vrf_group_public_key_bytes.to_vec(),
        vrf_public_polynomial_hash: outbe_primitives::proof::boundary_outcome_polynomial_hash(
            boundary.outcome.as_ref(),
        ),
    })
}

/// Returns whether `proposer` is in the active consensus set.
pub fn is_consensus_participant(storage: StorageHandle<'_>, proposer: Address) -> Result<bool> {
    ValidatorSet::new(storage).is_consensus_participant(proposer)
}

/// Returns whether `proposer` is a registered validator.
pub fn is_validator(storage: StorageHandle<'_>, proposer: Address) -> Result<bool> {
    ValidatorSet::new(storage).is_validator(proposer)
}

/// Accepts epoch 0 at block 1, or `current + 1` in steady state.
///
/// The boolean is `true` when the epoch height advances.
pub fn validate_boundary_epoch_transition(
    current_epoch: u64,
    incoming_epoch: u64,
    block_number: u64,
) -> Result<bool> {
    let regime = BlockHeightRegime::for_block(block_number);
    if regime.is_genesis_bootstrap() && current_epoch == 0 && incoming_epoch == 0 {
        return Ok(false);
    }
    let next_epoch = current_epoch
        .checked_add(1)
        .ok_or(BoundaryError::ValidatorSetEpochOverflow)?;
    if regime.is_steady_state() && incoming_epoch == next_epoch {
        return Ok(true);
    }
    Err(BoundaryError::InvalidBoundaryEpochTransition {
        current: current_epoch,
        incoming: incoming_epoch,
        block: block_number,
    }
    .into())
}

/// Hashes the ordered active-set addresses the way the boundary commitment does.
pub fn hash_boundary_active_set(addresses: &[Address]) -> B256 {
    let mut bytes = Vec::with_capacity(8 + addresses.len() * 20);
    bytes.extend_from_slice(&(addresses.len() as u64).to_be_bytes());
    for address in addresses {
        bytes.extend_from_slice(address.as_slice());
    }
    keccak256(bytes)
}

fn check_boundary_commitments(
    boundary: &DkgBoundaryArtifact,
    active_set_hash: B256,
    new_active_set: &[Address],
) -> Result<()> {
    let expected_tee_expiry_hash =
        outbe_primitives::reshare_artifact::tee_expired_target_exclusions_hash(
            &boundary.tee_expired_target_exclusions,
        )?;
    if boundary.tee_expired_target_exclusions_hash != expected_tee_expiry_hash {
        return Err(BoundaryError::TeeExpiryExclusionsHashMismatch {
            expected: expected_tee_expiry_hash,
            actual: boundary.tee_expired_target_exclusions_hash,
        }
        .into());
    }
    let expected_active_set_hash = hash_boundary_active_set(new_active_set);
    if active_set_hash != expected_active_set_hash {
        return Err(BoundaryError::ActiveSetHashMismatch {
            expected: expected_active_set_hash,
            actual: active_set_hash,
        }
        .into());
    }
    let expected_vrf_group_public_key = keccak256(boundary.vrf_group_public_key_bytes.as_ref());
    if boundary.vrf_group_public_key != expected_vrf_group_public_key {
        return Err(BoundaryError::VrfGroupPublicKeyHashMismatch {
            expected: expected_vrf_group_public_key,
            actual: boundary.vrf_group_public_key,
        }
        .into());
    }
    Ok(())
}

fn check_committee_set_hash(
    boundary: &DkgBoundaryArtifact,
    incoming_snapshot: &CommitteeSnapshot,
) -> Result<()> {
    let expected = crate::committee_set_hash_v2(boundary.epoch, incoming_snapshot);
    if boundary.committee_set_hash != expected {
        return Err(BoundaryError::CommitteeSetHashMismatch {
            expected,
            actual: boundary.committee_set_hash,
        }
        .into());
    }
    Ok(())
}

fn epoch_as_u64(number: impl TryInto<u64>) -> Result<u64> {
    number
        .try_into()
        .map_err(|_| BoundaryError::ValidatorSetEpochExceedsU64.into())
}

#[cfg(test)]
mod tests {
    use super::{hash_boundary_active_set, validate_boundary_epoch_transition};
    use alloy_primitives::{Address, B256};
    use outbe_primitives::error::PrecompileError;

    use crate::errors::BoundaryError;

    #[test]
    fn epoch_zero_at_block_one_does_not_advance() {
        let advances = validate_boundary_epoch_transition(0, 0, 1).unwrap();
        assert!(!advances);
    }

    #[test]
    fn steady_state_accepts_only_the_next_epoch() {
        assert!(validate_boundary_epoch_transition(3, 4, 2).unwrap());
        let error = validate_boundary_epoch_transition(3, 5, 2).unwrap_err();
        assert!(matches!(
            error,
            PrecompileError::Revert(message)
                if message.contains("current=3") && message.contains("incoming=5")
        ));
    }

    #[test]
    fn active_set_hash_binds_length_then_addresses() {
        let first = Address::repeat_byte(0x11);
        let second = Address::repeat_byte(0x22);
        let hashed = hash_boundary_active_set(&[first, second]);
        assert_ne!(hashed, B256::ZERO);
        assert_ne!(hashed, hash_boundary_active_set(&[second, first]));
    }

    #[test]
    fn epoch_overflow_is_fatal() {
        let error = validate_boundary_epoch_transition(u64::MAX, 0, 2).unwrap_err();
        assert!(matches!(error, PrecompileError::Fatal(_)));
        assert_eq!(
            error.to_string(),
            PrecompileError::from(BoundaryError::ValidatorSetEpochOverflow).to_string()
        );
    }
}
