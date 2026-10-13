//! Merkle-tree primitives.
//!
//! Shaped after Aztec's `noir-protocol-circuits/crates/types/src/merkle_tree`
//! (`merkle_tree.nr` + `root.nr`), trimmed to what this protocol needs: a
//! domain-separated inner-node hash and a root resolved from a sibling path.
//!
//! Aztec bakes the domain into a named hasher per tree (`merkle_hash`,
//! `nullifier_merkle_hash`, ...). Here the domain is a parameter instead, so
//! one function serves every tree — `EMIT_DOMAIN`, `DEMO_TRIBUTE_DOMAIN`, ...
//! Distinct domains are what keeps the trees separated.

use crate::hash::hash_3;

/// Canonical tree depth across the protocol — the chain's
/// `CommitmentWindowBase.TREE_DEPTH`, mirrored in Rust as
/// `outbe_zk::INCLUSION_DEPTH`.
pub global DEPTH: u32 = 32;

/// Inner-node hash: `Poseidon2(domain, left, right)`.
pub fn merkle_hash(domain: Field, left: Field, right: Field) -> Field {
    hash_3([domain, left, right])
}

/// Compute a Merkle root from a sibling path and explicit little-endian
/// left/right flags: `true` places the current node on the left.
pub fn merkle_root_from_indices<let N: u32>(
    domain: Field,
    leaf: Field,
    sibling_path: [Field; N],
    path_indices: [bool; N],
) -> Field {
    let mut node = leaf;

    for i in 0..N {
        let sibling = sibling_path[i];
        let (left, right) = if path_indices[i] {
            (node, sibling)
        } else {
            (sibling, node)
        };
        node = merkle_hash(domain, left, right);
    }

    node
}

/// Compute a Merkle root from a compact little-endian leaf index — Aztec's
/// `root_from_sibling_path`. A set bit means the node is the right child at
/// that level. `leaf_index` must fit in `N` bits; the decomposition enforces it.
pub fn merkle_root<let N: u32>(
    domain: Field,
    leaf: Field,
    leaf_index: Field,
    sibling_path: [Field; N],
) -> Field {
    let bits: [bool; N] = leaf_index.to_le_bits();
    let mut path_indices = [false; N];
    for i in 0..N {
        path_indices[i] = !bits[i];
    }
    merkle_root_from_indices(domain, leaf, sibling_path, path_indices)
}

#[test]
fn root_matches_hand_computed_depth_2() {
    let domain = 7;
    let leaf = 11;
    let siblings = [13, 17];

    // leaf_index 1 => right child at level 0, left child at level 1.
    let level_0 = merkle_hash(domain, siblings[0], leaf);
    let expected = merkle_hash(domain, level_0, siblings[1]);

    assert_eq(merkle_root(domain, leaf, 1, siblings), expected);
}

/// The two entry points must agree: the index form is the flag form with
/// `path_indices[i] == !bit_i(leaf_index)`.
#[test]
fn index_and_flag_forms_agree() {
    let domain = 7;
    let leaf = 11;
    let siblings = [13, 17, 19, 23];

    for index in 0..16 {
        let bits: [bool; 4] = (index as Field).to_le_bits();
        let mut flags = [false; 4];
        for i in 0..4 {
            flags[i] = !bits[i];
        }
        assert_eq(
            merkle_root(domain, leaf, index as Field, siblings),
            merkle_root_from_indices(domain, leaf, siblings, flags),
        );
    }
}

/// A leaf index wider than the tree cannot resolve a root.
#[test(should_fail)]
fn leaf_index_above_tree_capacity_fails() {
    let _ = merkle_root(7, 11, 16, [13, 17, 19, 23]);
}
