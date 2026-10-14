//! Typed error definitions for the L2Registry system precompile.

use alloy_primitives::Address;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Represents errors returned by the L2Registry runtime.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum L2RegistryError {
    #[error("invalid L2 registry proposal payload")]
    InvalidProposalPayload,

    #[error("L2 registry public key must be 0x-prefixed hex")]
    InvalidPublicKeyEncoding,

    #[error("chain id must be non-zero")]
    InvalidChainId,

    #[error("l1 address must be non-zero")]
    InvalidL1Address,

    #[error("inbox address must be non-zero")]
    InvalidInboxAddress,

    #[error("EIP-2537 G2 public key must be 256 bytes, got {length}")]
    InvalidPublicKeyLength { length: usize },

    #[error("BLS public key is not a canonical nonidentity EIP-2537 G2 group element")]
    InvalidPublicKey,

    #[error("inbox groupPubKey call failed")]
    InboxKeyCallFailed,

    #[error("L2 network {chain_id} is already registered")]
    NetworkAlreadyRegistered { chain_id: u64 },

    #[error("l1 address {l1_address} is already registered for chain {chain_id}")]
    L1AddressAlreadyRegistered { l1_address: Address, chain_id: u64 },

    #[error("L2 network {chain_id} is not registered")]
    NetworkNotRegistered { chain_id: u64 },

    #[error("caller {caller} is not the owner of L2 network {chain_id}")]
    NotNetworkOwner { caller: Address, chain_id: u64 },

    #[error("zkMerkleRoot must be exactly 32 bytes")]
    ZkMerkleRootRequired,

    #[error("invalid BLS signature over zkMerkleRoot")]
    InvalidZkSignature,
}

impl From<L2RegistryError> for PrecompileError {
    fn from(err: L2RegistryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}

impl L2RegistryError {
    pub fn into_execution_outcome(
        result: outbe_primitives::error::Result<()>,
    ) -> outbe_primitives::error::Result<outbe_vote::handlers::TargetExecutionOutcome> {
        use outbe_vote::handlers::TargetExecutionOutcome;
        match result {
            Ok(()) => Ok(TargetExecutionOutcome::Applied),
            Err(PrecompileError::Revert(reason)) => Ok(TargetExecutionOutcome::Error { reason }),
            Err(PrecompileError::RevertBytes(reason)) => Ok(TargetExecutionOutcome::Error {
                reason: format!("0x{}", hex::encode(reason)),
            }),
            Err(error) => Err(error),
        }
    }
}
