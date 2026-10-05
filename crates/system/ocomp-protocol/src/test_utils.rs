//! Real storage trie proof construction shared by test fixtures.

use std::collections::BTreeMap;

use alloy_primitives::{keccak256, Bytes, B256, U256};
use alloy_trie::{proof::ProofRetainer, HashBuilder, Nibbles};

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

    let mut builder =
        HashBuilder::default().with_proof_retainer(ProofRetainer::from_iter(targets.clone()));
    for (path, value) in leaves {
        builder.add_leaf(path, &value);
    }
    let root = builder.root();
    let retained = builder.take_proof_nodes();
    let proofs = targets
        .iter()
        .map(|target| {
            retained
                .matching_nodes_sorted(target)
                .into_iter()
                .map(|(_, node)| node)
                .collect()
        })
        .collect();
    (root, proofs)
}
