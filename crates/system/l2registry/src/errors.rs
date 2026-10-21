//! Typed error definitions for the L2Registry system precompile.

use crate::precompile::IL2Registry;
use alloy_primitives::Address;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
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
    fn from(error: L2RegistryError) -> Self {
        match error {
            L2RegistryError::InvalidProposalPayload => PrecompileError::RevertBytes(Bytes::from(
                IL2Registry::InvalidProposalPayload {}.abi_encode(),
            )),
            L2RegistryError::InvalidPublicKeyEncoding => PrecompileError::RevertBytes(Bytes::from(
                IL2Registry::InvalidPublicKeyEncoding {}.abi_encode(),
            )),
            L2RegistryError::InvalidChainId => PrecompileError::RevertBytes(Bytes::from(
                IL2Registry::InvalidChainId {}.abi_encode(),
            )),
            L2RegistryError::InvalidL1Address => PrecompileError::RevertBytes(Bytes::from(
                IL2Registry::InvalidL1Address {}.abi_encode(),
            )),
            L2RegistryError::InvalidInboxAddress => PrecompileError::RevertBytes(Bytes::from(
                IL2Registry::InvalidInboxAddress {}.abi_encode(),
            )),
            L2RegistryError::InvalidPublicKeyLength { length } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IL2Registry::InvalidPublicKeyLength {
                        length: length as u64,
                    }
                    .abi_encode(),
                ))
            }
            L2RegistryError::InvalidPublicKey => PrecompileError::RevertBytes(Bytes::from(
                IL2Registry::InvalidPublicKey {}.abi_encode(),
            )),
            L2RegistryError::InboxKeyCallFailed => PrecompileError::RevertBytes(Bytes::from(
                IL2Registry::InboxKeyCallFailed {}.abi_encode(),
            )),
            L2RegistryError::NetworkAlreadyRegistered { chain_id } => PrecompileError::RevertBytes(
                Bytes::from(IL2Registry::NetworkAlreadyRegistered { chain_id }.abi_encode()),
            ),
            L2RegistryError::L1AddressAlreadyRegistered {
                l1_address,
                chain_id,
            } => PrecompileError::RevertBytes(Bytes::from(
                IL2Registry::L1AddressAlreadyRegistered {
                    l1_address,
                    chain_id,
                }
                .abi_encode(),
            )),
            L2RegistryError::NetworkNotRegistered { chain_id } => PrecompileError::RevertBytes(
                Bytes::from(IL2Registry::NetworkNotRegistered { chain_id }.abi_encode()),
            ),
            L2RegistryError::NotNetworkOwner { caller, chain_id } => PrecompileError::RevertBytes(
                Bytes::from(IL2Registry::NotNetworkOwner { caller, chain_id }.abi_encode()),
            ),
            L2RegistryError::ZkMerkleRootRequired => PrecompileError::RevertBytes(Bytes::from(
                IL2Registry::ZkMerkleRootRequired {}.abi_encode(),
            )),
            L2RegistryError::InvalidZkSignature => PrecompileError::RevertBytes(Bytes::from(
                IL2Registry::InvalidZkSignature {}.abi_encode(),
            )),
        }
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

#[cfg(test)]
mod abi_tests {
    use super::*;

    fn encoded(err: L2RegistryError) -> Bytes {
        match PrecompileError::from(err) {
            PrecompileError::RevertBytes(bytes) => bytes,
            other => panic!("expected typed revert bytes, got {other:?}"),
        }
    }

    #[test]
    fn caller_revert_keeps_address_and_integer_fields() {
        let l1_address = Address::repeat_byte(0x11);
        let bytes = encoded(L2RegistryError::L1AddressAlreadyRegistered {
            l1_address,
            chain_id: 42161,
        });
        let decoded =
            IL2Registry::L1AddressAlreadyRegistered::abi_decode(&bytes).expect("address payload");
        assert_eq!(decoded.l1_address, l1_address);
        assert_eq!(decoded.chain_id, 42161);

        let bytes = encoded(L2RegistryError::InvalidPublicKeyLength { length: 33 });
        let decoded =
            IL2Registry::InvalidPublicKeyLength::abi_decode(&bytes).expect("length payload");
        assert_eq!(decoded.length, 33);
    }
}
