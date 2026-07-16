//! Narrow, non-consensus harness used only by reproducible ADR benchmarks.

use std::collections::BTreeMap;

use alloy_primitives::B256;

use crate::{
    persistence::{
        BranchKey, BranchNode, FieldValue, LeafValue, MergeValue, TreeKey as PersistedTreeKey,
    },
    smt::{PoseidonSmt, TreeKey, TreeLeaf, TreeProof, TreeRoot},
    ProvisionalTreeBatch, StagedTreeBatch, TreeChange,
};

pub struct Adr008SmtHarness {
    tree: PoseidonSmt,
}

pub struct Adr008Proof(TreeProof);

impl Adr008SmtHarness {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            tree: PoseidonSmt::empty(),
        }
    }

    pub fn root(&self) -> Result<[u8; 32], String> {
        self.tree
            .root()
            .map(TreeRoot::as_bytes)
            .map_err(|error| error.to_string())
    }

    pub fn update_all(&mut self, updates: &[([u8; 32], [u8; 32])]) -> Result<[u8; 32], String> {
        let updates = updates
            .iter()
            .map(|(key, leaf)| -> Result<_, String> {
                Ok((
                    TreeKey::from_be_bytes(*key).map_err(|error| error.to_string())?,
                    TreeLeaf::from_be_bytes(*leaf).map_err(|error| error.to_string())?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.tree
            .update_all(updates)
            .map(TreeRoot::as_bytes)
            .map_err(|error| error.to_string())
    }

    pub fn proof(&self, keys: &[[u8; 32]]) -> Result<Adr008Proof, String> {
        let keys = keys
            .iter()
            .copied()
            .map(|key| TreeKey::from_be_bytes(key).map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        self.tree
            .prove(keys)
            .map(Adr008Proof)
            .map_err(|error| error.to_string())
    }

    pub fn verify(
        &self,
        root: [u8; 32],
        proof: &Adr008Proof,
        leaves: &[([u8; 32], [u8; 32])],
    ) -> Result<(), String> {
        let root = TreeRoot::from_be_bytes(root).map_err(|error| error.to_string())?;
        let leaves = leaves
            .iter()
            .map(|(key, leaf)| -> Result<_, String> {
                Ok((
                    TreeKey::from_be_bytes(*key).map_err(|error| error.to_string())?,
                    TreeLeaf::from_be_bytes(*leaf).map_err(|error| error.to_string())?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.tree
            .verify(root, &proof.0, leaves)
            .map_err(|error| error.to_string())
    }
}

/// Builds a codec-valid staged batch for measuring the finalized MDBX apply
/// path. It intentionally does not assert any protocol capacity or timing.
pub fn staged_batch(
    block_number: u64,
    block_hash: B256,
    parent_block_hash: B256,
    parent_root: B256,
    new_root: B256,
    record_count: usize,
) -> Result<StagedTreeBatch, String> {
    let mut branches = BTreeMap::new();
    let mut leaves = BTreeMap::new();
    for index in 0..record_count {
        let ordinal = u64::try_from(index)
            .map_err(|_| "benchmark record count is not representable as u64".to_owned())?
            .saturating_add(1);
        let key_word = field_b256(ordinal);
        let value_word = field_b256(block_number.wrapping_add(ordinal).max(1));
        let field = FieldValue::try_from(value_word).map_err(|error| error.to_string())?;
        let branch_key =
            BranchKey::new(index as u8, key_word).map_err(|error| error.to_string())?;
        branches.insert(
            branch_key,
            TreeChange::Set(BranchNode {
                left: MergeValue::Value(field),
                right: MergeValue::Value(
                    FieldValue::try_from(B256::ZERO).map_err(|error| error.to_string())?,
                ),
            }),
        );
        leaves.insert(
            PersistedTreeKey::try_from(key_word).map_err(|error| error.to_string())?,
            TreeChange::Set(LeafValue::try_from(value_word).map_err(|error| error.to_string())?),
        );
    }
    ProvisionalTreeBatch::new(
        block_number,
        parent_block_hash,
        parent_root,
        new_root,
        branches,
        leaves,
    )
    .map(|batch| batch.freeze(block_hash))
    .map_err(|error| error.to_string())
}

#[must_use]
pub fn field_word(value: u64) -> [u8; 32] {
    let mut word = [0_u8; 32];
    word[24..].copy_from_slice(&value.to_be_bytes());
    word
}

#[must_use]
pub fn field_b256(value: u64) -> B256 {
    B256::from(field_word(value))
}
