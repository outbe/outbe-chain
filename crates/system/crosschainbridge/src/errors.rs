//! Typed error definitions for the CrosschainBridge system precompile.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

use crate::precompile::ICrosschainBridge;

/// Represents errors returned by the CrosschainBridge runtime.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CrosschainBridgeError {
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

impl From<CrosschainBridgeError> for PrecompileError {
    fn from(error: CrosschainBridgeError) -> Self {
        let bytes = match error {
            CrosschainBridgeError::NotOwnerCaller { caller } => {
                ICrosschainBridge::NotOwnerCaller { caller }.abi_encode()
            }
            CrosschainBridgeError::NotInitialized => {
                ICrosschainBridge::NotInitialized {}.abi_encode()
            }
            CrosschainBridgeError::InvalidAddress => {
                ICrosschainBridge::InvalidAddress {}.abi_encode()
            }
            CrosschainBridgeError::InvalidDomain => {
                ICrosschainBridge::InvalidDomain {}.abi_encode()
            }
            CrosschainBridgeError::LocalDomainNotAllowed { domain } => {
                ICrosschainBridge::LocalDomainNotAllowed { domain }.abi_encode()
            }
            CrosschainBridgeError::EmptyCalls => ICrosschainBridge::EmptyCalls {}.abi_encode(),
            CrosschainBridgeError::ZeroFund => ICrosschainBridge::ZeroFund {}.abi_encode(),
            CrosschainBridgeError::InsufficientBalance {
                required,
                available,
            } => ICrosschainBridge::InsufficientBalance {
                required,
                available,
            }
            .abi_encode(),
            CrosschainBridgeError::UndecodableReturn(value) => {
                ICrosschainBridge::UndecodableReturn {
                    value: value.to_string(),
                }
                .abi_encode()
            }
            CrosschainBridgeError::UnknownApprovedAction { proposal_id } => {
                ICrosschainBridge::UnknownApprovedAction {
                    proposalId: proposal_id,
                }
                .abi_encode()
            }
            CrosschainBridgeError::ApprovedPayloadMismatch { proposal_id } => {
                ICrosschainBridge::ApprovedPayloadMismatch {
                    proposalId: proposal_id,
                }
                .abi_encode()
            }
        };
        PrecompileError::RevertBytes(Bytes::from(bytes))
    }
}
