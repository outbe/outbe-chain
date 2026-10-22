use std::collections::BTreeMap;

use alloy_primitives::{Address, Bytes, B256, U256};
use reth_primitives_traits::Account;
use reth_trie::{AccountProof, StorageProof};
use serde::{Deserialize, Serialize};

use super::readonly::StorageReader;
use crate::error::{PrecompileError, Result};

/// Identifies the finalized state selected by a verified native proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalizedStateCheckpoint {
    pub block_number: u64,
    pub block_hash: B256,
    pub state_root: B256,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StorageSlotProof {
    pub slot: B256,
    pub value: U256,
    pub nodes: Vec<Bytes>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountStorageProof {
    pub address: Address,
    pub nonce: u64,
    pub balance: U256,
    pub code_hash: B256,
    pub storage_root: B256,
    pub account_nodes: Vec<Bytes>,
    pub slots: Vec<StorageSlotProof>,
}

#[derive(Clone, Copy, Debug)]
pub struct StateProofLimits {
    pub max_slots: usize,
    pub max_nodes: usize,
    pub max_bytes: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum StateProofError {
    #[error("state proof opens a different account")]
    WrongAccount,
    #[error("state proof does not contain an account")]
    MissingAccount,
    #[error("state proof exceeds its slot limit")]
    SlotLimit,
    #[error("state proof exceeds its node limit")]
    NodeLimit,
    #[error("state proof exceeds its byte limit")]
    ByteLimit,
    #[error("state proof slots are not strictly ordered")]
    SlotOrder,
    #[error("state proof contains an empty trie node")]
    EmptyNode,
    #[error("account proof verification failed: {0}")]
    Account(String),
    #[error("storage proof verification failed for {slot}: {reason}")]
    Slot { slot: B256, reason: String },
}

pub struct VerifiedStateSlots {
    address: Address,
    state_root: B256,
    slots: BTreeMap<B256, U256>,
}

impl VerifiedStateSlots {
    pub const fn state_root(&self) -> B256 {
        self.state_root
    }
}

impl StorageReader for VerifiedStateSlots {
    fn read_storage(&self, address: Address, key: B256) -> Result<U256> {
        if address != self.address {
            return Err(PrecompileError::storage(
                "storage read selects an unproven account",
            ));
        }
        self.slots.get(&key).copied().ok_or_else(|| {
            PrecompileError::storage(format!("storage read selects an unproven slot {key}"))
        })
    }
}

impl AccountStorageProof {
    pub fn verify(
        &self,
        expected_address: Address,
        finalized_state_root: B256,
        limits: StateProofLimits,
    ) -> std::result::Result<VerifiedStateSlots, StateProofError> {
        if self.address != expected_address {
            return Err(StateProofError::WrongAccount);
        }
        if self.slots.len() > limits.max_slots {
            return Err(StateProofError::SlotLimit);
        }
        if self
            .slots
            .windows(2)
            .any(|slots| slots[0].slot >= slots[1].slot)
        {
            return Err(StateProofError::SlotOrder);
        }
        self.validate_resource_limits(limits)?;
        AccountProof {
            address: self.address,
            info: Some(Account {
                nonce: self.nonce,
                balance: self.balance,
                bytecode_hash: Some(self.code_hash),
            }),
            storage_root: self.storage_root,
            proof: self.account_nodes.clone(),
            storage_proofs: Vec::new(),
        }
        .verify(finalized_state_root)
        .map_err(|error| StateProofError::Account(error.to_string()))?;
        let mut slots = BTreeMap::new();
        for opening in &self.slots {
            StorageProof {
                key: opening.slot,
                value: opening.value,
                ..StorageProof::new(opening.slot)
            }
            .with_proof(opening.nodes.clone())
            .verify(self.storage_root)
            .map_err(|error| StateProofError::Slot {
                slot: opening.slot,
                reason: error.to_string(),
            })?;
            slots.insert(opening.slot, opening.value);
        }
        Ok(VerifiedStateSlots {
            address: self.address,
            state_root: finalized_state_root,
            slots,
        })
    }

    fn validate_resource_limits(
        &self,
        limits: StateProofLimits,
    ) -> std::result::Result<(), StateProofError> {
        let mut bytes = self
            .slots
            .len()
            .checked_mul(64)
            .ok_or(StateProofError::ByteLimit)?;
        let mut nodes = 0usize;
        for node in self
            .account_nodes
            .iter()
            .chain(self.slots.iter().flat_map(|slot| slot.nodes.iter()))
        {
            if node.is_empty() {
                return Err(StateProofError::EmptyNode);
            }
            nodes = nodes.checked_add(1).ok_or(StateProofError::NodeLimit)?;
            bytes = bytes
                .checked_add(node.len())
                .ok_or(StateProofError::ByteLimit)?;
            if nodes > limits.max_nodes {
                return Err(StateProofError::NodeLimit);
            }
            if bytes > limits.max_bytes {
                return Err(StateProofError::ByteLimit);
            }
        }
        if bytes > limits.max_bytes {
            return Err(StateProofError::ByteLimit);
        }
        Ok(())
    }
}

impl TryFrom<&AccountProof> for AccountStorageProof {
    type Error = StateProofError;

    fn try_from(proof: &AccountProof) -> std::result::Result<Self, Self::Error> {
        let info = proof.info.as_ref().ok_or(StateProofError::MissingAccount)?;
        let mut slots: Vec<_> = proof
            .storage_proofs
            .iter()
            .map(|slot| StorageSlotProof {
                slot: slot.key,
                value: slot.value,
                nodes: slot.proof.clone(),
            })
            .collect();
        slots.sort_unstable_by_key(|slot| slot.slot);
        Ok(Self {
            address: proof.address,
            nonce: info.nonce,
            balance: info.balance,
            code_hash: info.get_bytecode_hash(),
            storage_root: proof.storage_root,
            account_nodes: proof.proof.clone(),
            slots,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::TrieAccount;
    use alloy_primitives::keccak256;
    use alloy_rlp::{Encodable, Header};

    fn leaf(key: B256, value: Vec<u8>) -> Bytes {
        let mut path = vec![0x20];
        path.extend_from_slice(key.as_slice());
        let path = Bytes::from(path);
        let value = Bytes::from(value);
        let mut encoded = Vec::new();
        Header {
            list: true,
            payload_length: path.length() + value.length(),
        }
        .encode(&mut encoded);
        path.encode(&mut encoded);
        value.encode(&mut encoded);
        encoded.into()
    }

    fn fixture() -> (B256, AccountStorageProof) {
        let address = Address::repeat_byte(1);
        let slot = B256::new(U256::from(16).to_be_bytes());
        let value = U256::from(42);
        let storage_node = leaf(keccak256(slot), alloy_rlp::encode(value));
        let storage_root = keccak256(&storage_node);
        let account = TrieAccount {
            nonce: 1,
            balance: U256::from(7),
            storage_root,
            code_hash: keccak256(b""),
        };
        let account_node = leaf(keccak256(address), alloy_rlp::encode(account));
        let state_root = keccak256(&account_node);
        (
            state_root,
            AccountStorageProof {
                address,
                nonce: account.nonce,
                balance: account.balance,
                code_hash: account.code_hash,
                storage_root,
                account_nodes: vec![account_node],
                slots: vec![StorageSlotProof {
                    slot,
                    value,
                    nodes: vec![storage_node],
                }],
            },
        )
    }

    fn limits() -> StateProofLimits {
        StateProofLimits {
            max_slots: 16,
            max_nodes: 64,
            max_bytes: 16_384,
        }
    }

    #[test]
    fn valid_account_and_storage_paths_open_only_proven_slots() {
        let (root, proof) = fixture();
        let verified = proof.verify(proof.address, root, limits()).unwrap();
        assert_eq!(verified.state_root(), root);
        assert_eq!(
            verified
                .read_storage(proof.address, proof.slots[0].slot)
                .unwrap(),
            U256::from(42)
        );
        assert!(verified
            .read_storage(Address::repeat_byte(2), proof.slots[0].slot)
            .is_err());
        assert!(verified.read_storage(proof.address, B256::ZERO).is_err());
    }

    #[test]
    fn authenticated_absence_is_distinct_from_an_omitted_slot() {
        let (root, mut proof) = fixture();
        let absent = B256::new(U256::from(17).to_be_bytes());
        proof.slots.push(StorageSlotProof {
            slot: absent,
            value: U256::ZERO,
            nodes: proof.slots[0].nodes.clone(),
        });
        let verified = proof.verify(proof.address, root, limits()).unwrap();
        assert_eq!(
            verified.read_storage(proof.address, absent).unwrap(),
            U256::ZERO
        );
    }

    #[test]
    fn altered_account_root_slot_value_and_truncated_nodes_are_rejected() {
        let (root, proof) = fixture();
        assert!(proof
            .verify(Address::repeat_byte(2), root, limits())
            .is_err());
        assert!(proof.verify(proof.address, B256::ZERO, limits()).is_err());
        let mut changed = proof.clone();
        changed.slots[0].value += U256::from(1);
        assert!(changed.verify(changed.address, root, limits()).is_err());
        let mut changed = proof.clone();
        changed.slots[0].slot = B256::ZERO;
        assert!(changed.verify(changed.address, root, limits()).is_err());
        let mut changed = proof.clone();
        changed.account_nodes.clear();
        assert!(changed.verify(changed.address, root, limits()).is_err());
        let mut changed = proof.clone();
        changed.slots[0].nodes.clear();
        assert!(changed.verify(changed.address, root, limits()).is_err());
    }

    #[test]
    fn duplicate_slots_and_resource_limits_are_rejected() {
        let (root, mut proof) = fixture();
        for bound in [
            StateProofLimits {
                max_slots: 0,
                ..limits()
            },
            StateProofLimits {
                max_nodes: 0,
                ..limits()
            },
            StateProofLimits {
                max_bytes: 1,
                ..limits()
            },
        ] {
            assert!(proof.verify(proof.address, root, bound).is_err());
        }
        proof.slots.push(proof.slots[0].clone());
        assert!(matches!(
            proof.verify(proof.address, root, limits()),
            Err(StateProofError::SlotOrder)
        ));
    }
}
