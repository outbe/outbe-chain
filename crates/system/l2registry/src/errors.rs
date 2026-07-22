use alloy_primitives::Address;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum L2RegistryError {
    #[error("chain id must be non-zero")]
    InvalidChainId,

    #[error("l1 address must be non-zero")]
    InvalidL1Address,

    #[error("BLS public key must be 48 bytes, got {length}")]
    InvalidPublicKeyLength { length: usize },

    #[error("BLS public key is not a valid MinPk group element")]
    InvalidPublicKey,

    #[error("L2 network {chain_id} is already registered")]
    NetworkAlreadyRegistered { chain_id: u64 },

    #[error("l1 address {l1_address} is already registered for chain {chain_id}")]
    L1AddressAlreadyRegistered { l1_address: Address, chain_id: u64 },

    #[error("L2 network {chain_id} is not registered")]
    NetworkNotRegistered { chain_id: u64 },

    #[error("zkMerkleRoot is required when ZK verification is enabled")]
    ZkMerkleRootRequired,

    #[error("invalid BLS signature over zkMerkleRoot")]
    InvalidZkSignature,
}

impl From<L2RegistryError> for PrecompileError {
    fn from(err: L2RegistryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
