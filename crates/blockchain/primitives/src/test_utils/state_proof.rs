//! Independent trie builders for native state proof tests.

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_trie::{
    proof::{ProofNodes, ProofRetainer},
    HashBuilder, Nibbles, TrieAccount,
};
use std::collections::BTreeMap;

/// Collect the retained proof nodes for a target in canonical path order.
pub fn proof_nodes_for_target(retained: &ProofNodes, target: &Nibbles) -> Vec<Bytes> {
    retained
        .matching_nodes_sorted(target)
        .into_iter()
        .map(|(_, node)| node)
        .collect()
}

/// Insert ordered leaves and retain proof nodes for the targets.
pub fn trie_root_with_proof_nodes(
    targets: impl IntoIterator<Item = Nibbles>,
    leaves: impl IntoIterator<Item = (Nibbles, Vec<u8>)>,
) -> (B256, ProofNodes) {
    let mut builder = HashBuilder::default().with_proof_retainer(ProofRetainer::from_iter(targets));
    for (path, value) in leaves {
        builder.add_leaf(path, &value);
    }
    let root = builder.root();
    let retained = builder.take_proof_nodes();
    (root, retained)
}

/// Build an account trie and return each proof by account address.
pub fn account_mpt_with_proofs(
    accounts: &[(Address, TrieAccount)],
) -> (B256, BTreeMap<Address, Vec<Bytes>>) {
    let targets = accounts
        .iter()
        .map(|(address, _)| (*address, Nibbles::unpack(keccak256(address))))
        .collect::<BTreeMap<_, _>>();
    let mut leaves = accounts
        .iter()
        .map(|(address, account)| (targets[address], alloy_rlp::encode(*account)))
        .collect::<Vec<_>>();
    leaves.sort_by_key(|(path, _)| *path);
    let (root, retained) = trie_root_with_proof_nodes(targets.values().copied(), leaves);
    let proofs = targets
        .into_iter()
        .map(|(address, target)| {
            let proof = proof_nodes_for_target(&retained, &target);
            (address, proof)
        })
        .collect();
    (root, proofs)
}

/// Build canonical storage-root proofs in the supplied slot order.
pub fn storage_trie(slots: &[(U256, U256)]) -> (B256, Vec<Vec<Bytes>>) {
    let targets = slots
        .iter()
        .map(|(slot, _)| Nibbles::unpack(keccak256(slot.to_be_bytes::<32>())))
        .collect::<Vec<_>>();
    let mut leaves = BTreeMap::new();
    for ((_, word), target) in slots.iter().zip(&targets) {
        if !word.is_zero() {
            leaves.insert(*target, alloy_rlp::encode_fixed_size(word).to_vec());
        }
    }

    let (root, retained) = trie_root_with_proof_nodes(targets.clone(), leaves);
    let proofs = targets
        .iter()
        .map(|target| proof_nodes_for_target(&retained, target))
        .collect();
    (root, proofs)
}

/// Encode Solidity bytes independently of the production storage encoder.
pub fn solidity_bytes_storage_slots(base: U256, encoded: &[u8]) -> Vec<(U256, U256)> {
    if encoded.len() <= 31 {
        let mut inline = [0_u8; 32];
        inline[..encoded.len()].copy_from_slice(encoded);
        inline[31] = (encoded.len() * 2) as u8;
        return vec![(base, U256::from_be_bytes(inline))];
    }

    let data_base = U256::from_be_bytes(keccak256(base.to_be_bytes::<32>()).0);
    std::iter::once((base, U256::from(encoded.len() * 2 + 1)))
        .chain(encoded.chunks(32).enumerate().map(|(index, chunk)| {
            let mut word = [0_u8; 32];
            word[..chunk.len()].copy_from_slice(chunk);
            (data_base + U256::from(index), U256::from_be_bytes(word))
        }))
        .collect()
}
