//! Native account and exact-slot storage proof verification.

use alloy_primitives::{Address, Bytes, B256, U256};
use reth_primitives_traits::Account;
use reth_trie::{AccountProof, StorageProof};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicStorageProofV1 {
    pub key: B256,
    pub value: U256,
    pub nodes: Vec<Bytes>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicAccountProofV1 {
    pub address: Address,
    pub nonce: u64,
    pub balance: U256,
    pub storage_root: B256,
    pub code_hash: B256,
    pub account_nodes: Vec<Bytes>,
    pub storage_proofs: Vec<PublicStorageProofV1>,
}

impl PublicAccountProofV1 {
    pub(crate) fn to_reth_proof(&self) -> AccountProof {
        AccountProof {
            address: self.address,
            info: Some(Account {
                nonce: self.nonce,
                balance: self.balance,
                bytecode_hash: Some(self.code_hash),
            }),
            proof: self.account_nodes.clone(),
            storage_root: self.storage_root,
            storage_proofs: self
                .storage_proofs
                .iter()
                .map(|proof| {
                    StorageProof {
                        key: proof.key,
                        value: proof.value,
                        ..StorageProof::new(proof.key)
                    }
                    .with_proof(proof.nodes.clone())
                })
                .collect(),
        }
    }
}

pub(crate) fn verify_public_account_proof(
    proof: &PublicAccountProofV1,
    expected_address: Address,
    expected_slots: &[B256],
    state_root: B256,
) -> Result<(), PublicStateProofError> {
    if proof.address != expected_address {
        return Err(PublicStateProofError::ProofAddress);
    }
    if proof.storage_proofs.len() != expected_slots.len()
        || expected_slots.iter().any(|slot| {
            proof
                .storage_proofs
                .iter()
                .filter(|candidate| candidate.key == *slot)
                .count()
                != 1
        })
    {
        return Err(PublicStateProofError::ProofSlotSet);
    }
    let reth_proof = proof.to_reth_proof();
    reth_proof
        .verify(state_root)
        .map_err(|error| PublicStateProofError::AccountProof(error.to_string()))?;
    for storage_proof in &reth_proof.storage_proofs {
        storage_proof
            .verify(proof.storage_root)
            .map_err(|error| PublicStateProofError::StorageProof(error.to_string()))?;
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum PublicStateProofError {
    #[error("public account proof belongs to another address")]
    ProofAddress,
    #[error("public account proof returned a different storage-slot set")]
    ProofSlotSet,
    #[error("public account proof rejected: {0}")]
    AccountProof(String),
    #[error("public storage proof rejected: {0}")]
    StorageProof(String),
}
