//! Typed error definitions for the CrosschainGov system precompile.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

use crate::precompile::ICrosschainGov;

/// Represents errors returned by the CrosschainGov runtime.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CrosschainGovError {
    #[error("caller {caller} may not make owner calls")]
    NotOwnerCaller { caller: Address },

    #[error("the Hyperlane registry is not initialized")]
    NotInitialized,

    #[error("address must be non-zero")]
    InvalidAddress,

    #[error("domain must be non-zero")]
    InvalidDomain,

    #[error("local domain {domain} cannot be targeted remotely")]
    LocalDomainNotAllowed { domain: u32 },

    #[error("remote call list must be non-empty")]
    EmptyCalls,

    #[error("fund requires a non-zero value")]
    ZeroFund,

    #[error("owner balance {available} is below the required {required}")]
    InsufficientBalance { required: U256, available: U256 },

    #[error("undecodable return data from {0}")]
    UndecodableReturn(&'static str),

    #[error("no approved action for proposal {proposal_id}")]
    UnknownApprovedAction { proposal_id: U256 },

    #[error("payload does not match the approved action of proposal {proposal_id}")]
    ApprovedPayloadMismatch { proposal_id: U256 },
}

impl From<CrosschainGovError> for PrecompileError {
    fn from(error: CrosschainGovError) -> Self {
        let bytes = match error {
            CrosschainGovError::NotOwnerCaller { caller } => {
                ICrosschainGov::NotOwnerCaller { caller }.abi_encode()
            }
            CrosschainGovError::NotInitialized => ICrosschainGov::NotInitialized {}.abi_encode(),
            CrosschainGovError::InvalidAddress => ICrosschainGov::InvalidAddress {}.abi_encode(),
            CrosschainGovError::InvalidDomain => ICrosschainGov::InvalidDomain {}.abi_encode(),
            CrosschainGovError::LocalDomainNotAllowed { domain } => {
                ICrosschainGov::LocalDomainNotAllowed { domain }.abi_encode()
            }
            CrosschainGovError::EmptyCalls => ICrosschainGov::EmptyCalls {}.abi_encode(),
            CrosschainGovError::ZeroFund => ICrosschainGov::ZeroFund {}.abi_encode(),
            CrosschainGovError::InsufficientBalance {
                required,
                available,
            } => ICrosschainGov::InsufficientBalance {
                required,
                available,
            }
            .abi_encode(),
            CrosschainGovError::UndecodableReturn(value) => ICrosschainGov::UndecodableReturn {
                value: value.to_string(),
            }
            .abi_encode(),
            CrosschainGovError::UnknownApprovedAction { proposal_id } => {
                ICrosschainGov::UnknownApprovedAction {
                    proposalId: proposal_id,
                }
                .abi_encode()
            }
            CrosschainGovError::ApprovedPayloadMismatch { proposal_id } => {
                ICrosschainGov::ApprovedPayloadMismatch {
                    proposalId: proposal_id,
                }
                .abi_encode()
            }
        };
        PrecompileError::RevertBytes(Bytes::from(bytes))
    }
}
