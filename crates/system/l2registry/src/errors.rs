use alloy_primitives::Address;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

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

    #[error("EIP-2537 G2 public key must be 256 bytes, got {length}")]
    InvalidPublicKeyLength { length: usize },

    #[error("BLS public key is not a canonical nonidentity EIP-2537 G2 group element")]
    InvalidPublicKey,

    #[error("inbox groupPubKey must return a valid 256-byte EIP-2537 G2 public key")]
    InvalidInboxPublicKey,

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
