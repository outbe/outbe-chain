//! Domain error types for the validator set precompile and runtime.

mod precompile;
mod set;

pub use set::ValidatorSetError;

use crate::precompile::IValidatorSet;
use alloy_primitives::Bytes;
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
