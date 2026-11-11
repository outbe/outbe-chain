//! Incremental Merkle tree of the Hyperlane `MerkleTreeHook` (depth 32).
//!
//! Ported from hyperlane-monorepo `agents-v3.0.0`,
//! `rust/main/hyperlane-core/src/accumulator/incremental.rs` (Apache-2.0, see
//! `LICENSE-APACHE`). Changes: alloy types, zero hashes computed once at
//! startup, no proofs, no Borsh snapshot.

use std::sync::LazyLock;

use alloy_primitives::{keccak256, B256};

/// Depth of the `MerkleTreeHook` tree.
pub const TREE_DEPTH: usize = 32;

/// `ZERO_HASHES[i]` is the root of an empty subtree of height `i`.
static ZERO_HASHES: LazyLock<[B256; TREE_DEPTH + 1]> = LazyLock::new(|| {
    let mut hashes = [B256::ZERO; TREE_DEPTH + 1];
    for i in 0..TREE_DEPTH {
        hashes[i + 1] = hash_concat(hashes[i], hashes[i]);
    }
    hashes
});

fn hash_concat(left: B256, right: B256) -> B256 {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(left.as_slice());
    buf[32..].copy_from_slice(right.as_slice());
    keccak256(buf)
}

/// The tree keeps only the left frontier: O(depth) memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncrementalMerkle {
    branch: [B256; TREE_DEPTH],
    count: usize,
}

impl Default for IncrementalMerkle {
    fn default() -> Self {
        let mut branch = [B256::ZERO; TREE_DEPTH];
        branch.copy_from_slice(&ZERO_HASHES[..TREE_DEPTH]);
        Self { branch, count: 0 }
    }
}

impl IncrementalMerkle {
    /// Adds a leaf (a Hyperlane message id).
    ///
    /// # Panics
    /// Panics when the tree already holds `u32::MAX` leaves. The contract
    /// cannot hold more leaves, so this cannot occur on a correct chain.
    pub fn ingest(&mut self, leaf: B256) {
        assert!(self.count < u32::MAX as usize, "merkle tree is full");
        self.count += 1;
        let mut node = leaf;
        let mut size = self.count;
        for i in 0..TREE_DEPTH {
            if size & 1 == 1 {
                self.branch[i] = node;
                return;
            }
            node = hash_concat(self.branch[i], node);
            size /= 2;
        }
    }

    /// Returns the current root. Equal to `MerkleTreeHook.root()` after the
    /// same leaves.
    pub fn root(&self) -> B256 {
        // Each low zero bit of `count` hashes an empty subtree with itself.
        // `ZERO_HASHES` already holds these nodes.
        let skip = self.count.trailing_zeros() as usize;
        if skip >= TREE_DEPTH {
            return ZERO_HASHES[TREE_DEPTH];
        }
        let mut node = ZERO_HASHES[skip];
        let mut size = self.count >> skip;
        for (i, elem) in self.branch.iter().enumerate().skip(skip) {
            node = if size & 1 == 1 {
                hash_concat(*elem, node)
            } else {
                hash_concat(node, ZERO_HASHES[i])
            };
            size /= 2;
        }
        node
    }

    /// Returns the number of leaves.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Returns the index of the last leaf, or `None` for an empty tree.
    pub fn index(&self) -> Option<u32> {
        self.count.checked_sub(1).map(|i| i as u32)
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{b256, eip191_hash_message};
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Case {
        test_name: String,
        expected_root: B256,
        leaves: Vec<String>,
    }

    #[test]
    fn empty_root_matches_hyperlane_initial_root() {
        assert_eq!(
            IncrementalMerkle::default().root(),
            b256!("0x27ae5ba08d7291c96c8cbddcc148bf48a6d68c7974b94356f53754ef6171d757")
        );
        assert_eq!(IncrementalMerkle::default().index(), None);
    }

    /// Upstream `vectors/merkle.json`: leaves are EIP-191 hashes of strings.
    #[test]
    fn upstream_vectors() {
        let cases: Vec<Case> =
            serde_json::from_str(include_str!("../testdata/merkle.json")).unwrap();
        for case in cases {
            let mut tree = IncrementalMerkle::default();
            for leaf in &case.leaves {
                tree.ingest(eip191_hash_message(leaf));
            }
            assert_eq!(tree.count(), case.leaves.len(), "{}", case.test_name);
            assert_eq!(tree.root(), case.expected_root, "{}", case.test_name);
        }
    }

    /// Outbe testnet `MerkleTreeHook` `InsertedIntoTree` leaves. The roots
    /// are the signed checkpoints (34, 35) and `latestCheckpoint()` at 42.
    #[test]
    fn outbe_testnet_hook_roots() {
        let vector: serde_json::Value =
            serde_json::from_str(include_str!("../testdata/outbe-testnet-checkpoints.json"))
                .unwrap();
        let leaves: Vec<B256> = serde_json::from_value(vector["leaves"].clone()).unwrap();
        assert_eq!(leaves.len(), 43);
        let checkpoints = vector["checkpoints"].as_array().unwrap();
        let mut tree = IncrementalMerkle::default();
        for leaf in &leaves {
            tree.ingest(*leaf);
            for c in checkpoints {
                if c["index"].as_u64().map(|i| i as u32) == tree.index() {
                    let root: B256 = serde_json::from_value(c["root"].clone()).unwrap();
                    assert_eq!(tree.root(), root, "index {:?}", tree.index());
                    assert_eq!(c["message_id"].as_str().unwrap(), leaf.to_string());
                }
            }
        }
        assert_eq!(tree.index(), Some(42));
        assert_eq!(
            tree.root(),
            b256!("0x139623eb9477f5967bdfdbe2932a93e6460b65156e39fc15e9ddc4f0f30e1e39")
        );
    }
}
