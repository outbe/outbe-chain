//! Domain error types for the validator set precompile and runtime.

mod precompile;
mod set;

pub use set::ValidatorSetError;

use crate::precompile::IValidatorSet;
use alloy_primitives::{Address, Bytes, B256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Deterministic activation failures for the validator set.
///
/// `next_vrf_material_version` returns this error when the version overflows.
/// `activate_reshared_set` is a test helper and does not return this error.
/// The production boundary hook does not return this error either.
/// Consensus can reject the activation and avoid a node panic.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ActivationError {
    /// `vrf_material_version` reached `u64::MAX` and cannot increment.
    ///
    /// Activation must reject deterministically instead of saturating.
    /// Proposer and validator paths then observe the same failure.
    #[error("vrf material version overflow at reshare activation")]
    VrfVersionOverflow,
}

impl From<outbe_primitives::validator_dto::VrfVersionOverflow> for ActivationError {
    fn from(_error: outbe_primitives::validator_dto::VrfVersionOverflow) -> Self {
        Self::VrfVersionOverflow
    }
}

impl From<ActivationError> for PrecompileError {
    fn from(error: ActivationError) -> Self {
        match error {
            ActivationError::VrfVersionOverflow => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::VrfVersionOverflow {}.abi_encode(),
            )),
        }
    }
}

/// Failures of a certified boundary transition.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BoundaryError {
    /// Committee metadata lists the same address twice.
    #[error("consensus metadata committee contains duplicate addresses")]
    DuplicateCommitteeAddresses,
    /// TEE expiry exclusion commitment does not match the listed addresses.
    #[error(
        "boundary TEE expiry exclusions commitment mismatch: expected {expected}, got {actual}"
    )]
    TeeExpiryExclusionsHashMismatch {
        /// Hash of the exclusion list.
        expected: B256,
        /// Commitment carried by the boundary.
        actual: B256,
    },
    /// Active-set hash does not match the ordered address list.
    #[error("boundary active_set_hash mismatch: expected {expected}, got {actual}")]
    ActiveSetHashMismatch {
        /// Hash of `new_active_set`.
        expected: B256,
        /// Hash carried by the reshare.
        actual: B256,
    },
    /// VRF group public key hash does not match the key bytes.
    #[error("boundary VRF group public key hash mismatch: expected {expected}, got {actual}")]
    VrfGroupPublicKeyHashMismatch {
        /// Hash of the key bytes.
        expected: B256,
        /// Hash carried by the boundary.
        actual: B256,
    },
    /// Committee snapshot hash does not match the derived snapshot.
    #[error("boundary committee_set_hash mismatch: expected {expected}, got {actual}")]
    CommitteeSetHashMismatch {
        /// Hash of the derived snapshot.
        expected: B256,
        /// Hash carried by the boundary.
        actual: B256,
    },
    /// Stored epoch does not fit in `u64`.
    #[error("ValidatorSet epoch exceeds u64")]
    ValidatorSetEpochExceedsU64,
    /// Active-set hash changed but the boundary is not a set change.
    #[error(
        "boundary active_set_hash changed without validator-set change: current={current}, boundary={boundary}"
    )]
    ActiveSetHashChangedWithoutValidatorSetChange {
        /// Hash stored on the validator set.
        current: B256,
        /// Hash carried by the reshare.
        boundary: B256,
    },
    /// Adding one to the current epoch overflowed.
    #[error("ValidatorSet epoch overflow at BoundaryOutcome")]
    ValidatorSetEpochOverflow,
    /// Incoming epoch is neither the block-1 bootstrap nor `current + 1`.
    #[error(
        "BoundaryOutcome epoch must bootstrap epoch 0 at block 1 or activate current+1: current={current}, incoming={incoming}, block={block}"
    )]
    InvalidBoundaryEpochTransition {
        /// Epoch stored before the boundary.
        current: u64,
        /// Epoch named by the boundary.
        incoming: u64,
        /// Block that carries the boundary.
        block: u64,
    },
    /// An address in the new active set has no consensus key.
    #[error("boundary active set contains unregistered validator {address}")]
    UnregisteredBoundaryValidator {
        /// Address missing from the validator set.
        address: Address,
    },
}

impl From<BoundaryError> for PrecompileError {
    fn from(error: BoundaryError) -> Self {
        match error {
            BoundaryError::ValidatorSetEpochExceedsU64
            | BoundaryError::ValidatorSetEpochOverflow => PrecompileError::Fatal(error.to_string()),
            BoundaryError::DuplicateCommitteeAddresses
            | BoundaryError::TeeExpiryExclusionsHashMismatch { .. }
            | BoundaryError::ActiveSetHashMismatch { .. }
            | BoundaryError::VrfGroupPublicKeyHashMismatch { .. }
            | BoundaryError::CommitteeSetHashMismatch { .. }
            | BoundaryError::ActiveSetHashChangedWithoutValidatorSetChange { .. }
            | BoundaryError::InvalidBoundaryEpochTransition { .. }
            | BoundaryError::UnregisteredBoundaryValidator { .. } => {
                PrecompileError::Revert(error.to_string())
            }
        }
    }
}
