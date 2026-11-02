//! Typed error definitions for the CrosschainGov precompile.
//!
//! The Hyperlane errors come from `outbe_hyperlanecontroller::errors`. Both
//! map to the errors of `ICrosschainGov.sol`.

use alloy_primitives::{Bytes, U256};
use alloy_sol_types::SolError;
use outbe_hyperlanecontroller::abi::ICrosschainGov;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Represents errors returned by the CrosschainGov runtime.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CrosschainGovError {
    #[error("address must be non-zero")]
    InvalidAddress,

    #[error("remote call list must be non-empty")]
    EmptyCalls,

    #[error("fund requires a non-zero value")]
    ZeroFund,

    #[error("precompile balance {available} is below the required {required}")]
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
            CrosschainGovError::InvalidAddress => ICrosschainGov::InvalidAddress {}.abi_encode(),
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
