//! Typed error definitions for the AttestersRegistry precompile.

use alloy_primitives::{Address, Bytes};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

use crate::precompile::IAttestersRegistry;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AttestersRegistryError {
    #[error("caller is not admin")]
    NotAdmin,

    #[error("attester {attester} is already registered")]
    AlreadyRegistered { attester: Address },

    #[error("attester {attester} is not registered")]
    NotRegistered { attester: Address },

    #[error("attester {attester} is not an active rotation candidate")]
    NotRotationCandidate { attester: Address },

    #[error("attested generation {attested} does not match requested generation {requested}")]
    WrongGeneration { attested: u64, requested: u64 },

    #[error("generation {generation} is already active")]
    GenerationAlreadyActive { generation: u64 },

    #[error("attester {attester} already attested generation {generation}")]
    AlreadyAttested { generation: u64, attester: Address },

    #[error("output hash must be non-zero")]
    ZeroOutputHash,

    #[error("address must be non-zero")]
    ZeroAddress,

    #[error("permission mask must be non-zero")]
    InvalidMask,

    #[error("AttestersRegistry: rotation candidate needs consensusKey")]
    RotationCandidateNeedsConsensusKey,
}

impl From<AttestersRegistryError> for PrecompileError {
    fn from(error: AttestersRegistryError) -> Self {
        match error {
            AttestersRegistryError::NotAdmin => PrecompileError::RevertBytes(Bytes::from(
                IAttestersRegistry::NotAdmin {}.abi_encode(),
            )),
            AttestersRegistryError::AlreadyRegistered { attester } => PrecompileError::RevertBytes(
                Bytes::from(IAttestersRegistry::AlreadyRegistered { attester }.abi_encode()),
            ),
            AttestersRegistryError::NotRegistered { attester } => PrecompileError::RevertBytes(
                Bytes::from(IAttestersRegistry::NotRegistered { attester }.abi_encode()),
            ),
            AttestersRegistryError::NotRotationCandidate { attester } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IAttestersRegistry::NotRotationCandidate { attester }.abi_encode(),
                ))
            }
            AttestersRegistryError::WrongGeneration {
                attested,
                requested,
            } => PrecompileError::RevertBytes(Bytes::from(
                IAttestersRegistry::WrongGeneration {
                    attested,
                    requested,
                }
                .abi_encode(),
            )),
            AttestersRegistryError::GenerationAlreadyActive { generation } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IAttestersRegistry::GenerationAlreadyActive { generation }.abi_encode(),
                ))
            }
            AttestersRegistryError::AlreadyAttested {
                generation,
                attester,
            } => PrecompileError::RevertBytes(Bytes::from(
                IAttestersRegistry::AlreadyAttested {
                    generation,
                    attester,
                }
                .abi_encode(),
            )),
            AttestersRegistryError::ZeroOutputHash => PrecompileError::RevertBytes(Bytes::from(
                IAttestersRegistry::ZeroOutputHash {}.abi_encode(),
            )),
            AttestersRegistryError::ZeroAddress => PrecompileError::RevertBytes(Bytes::from(
                IAttestersRegistry::ZeroAddress {}.abi_encode(),
            )),
            AttestersRegistryError::InvalidMask => PrecompileError::RevertBytes(Bytes::from(
                IAttestersRegistry::InvalidMask {}.abi_encode(),
            )),
            AttestersRegistryError::RotationCandidateNeedsConsensusKey => {
                PrecompileError::Revert(error.to_string())
            }
        }
    }
}
