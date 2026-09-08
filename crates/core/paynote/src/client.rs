//! Off-chain membership witnesses for the PayNote commitment tree.
//!
//! Feed commitments from ordered `NewNote` events into [`Tree`]. The caller
//! must validate event indexes and compare the resulting root with the pool's
//! on-chain root before using a witness. RPC and note-secret storage belong to
//! the client application.

use crate::{
    errors::PayNoteError,
    hash::{empty_subtrees, merkle_node, Field},
    schema::PAYNOTE_TREE_DEPTH,
};

/// A reconstructed depth-32 commitment tree with membership-path generation.
///
/// Uses the same hash functions and empty-subtree ladder as the runtime.
/// Leaves are retained to derive paths for any deposited or change note.
pub struct Tree {
    leaves: Vec<Field>,
    zeros: Vec<Field>,
    frontier: [Field; PAYNOTE_TREE_DEPTH],
    root: Field,
}

impl Tree {
    /// Start with the chain-specific empty tree.
    pub fn new(chain_id: u64) -> Result<Self, PayNoteError> {
        let zeros = empty_subtrees(chain_id, PAYNOTE_TREE_DEPTH)?;
        Ok(Self {
            leaves: Vec::new(),
            root: zeros[PAYNOTE_TREE_DEPTH],
            zeros,
            frontier: [Field::from(0); PAYNOTE_TREE_DEPTH],
        })
    }

    /// Commitments in append order. Returned read-only to preserve tree consistency.
    pub fn leaves(&self) -> &[Field] {
        &self.leaves
    }

    /// Root after all appended commitments, or the empty root before any append.
    pub fn root(&self) -> Field {
        self.root
    }

    /// Append the next commitment from the event history.
    ///
    /// Returns an error without changing the tree if hashing fails or the
    /// depth-32 tree is full.
    pub fn append(&mut self, commitment: Field) -> Result<(), PayNoteError> {
        // Circuit positions are u32; reject beyond the depth-32 tree capacity.
        let mut index = u32::try_from(self.leaves.len()).map_err(|_| PayNoteError::TreeFull)?;
        let mut frontier = self.frontier;
        let mut node = commitment;
        for (level, zero) in self.zeros.iter().enumerate().take(PAYNOTE_TREE_DEPTH) {
            node = if index & 1 == 0 {
                frontier[level] = node;
                merkle_node(node, *zero)?
            } else {
                merkle_node(frontier[level], node)?
            };
            index >>= 1;
        }
        self.leaves.push(commitment);
        self.frontier = frontier;
        self.root = node;
        Ok(())
    }

    /// Return the leaf index and 32 siblings for a commitment under [`Self::root`].
    ///
    /// Siblings are ordered from leaf level upwards, matching the spend circuit's
    /// `auth_path`. Missing commitments return an error, including change notes
    /// whose creating spend has not yet appeared in the event history.
    pub fn witness(
        &self,
        commitment: Field,
    ) -> Result<(u32, [Field; PAYNOTE_TREE_DEPTH]), PayNoteError> {
        let position = self
            .leaves
            .iter()
            .position(|leaf| *leaf == commitment)
            .ok_or_else(|| {
                PayNoteError::InvalidInput(
                    "note commitment is not on-chain; deposit or change is not yet confirmed"
                        .into(),
                )
            })?;
        let leaf_index = u32::try_from(position).map_err(|_| PayNoteError::TreeFull)?;
        let mut index = position;
        // ponytail: O(leaves) memory and hashing per witness; cache tree levels when pool size warrants it.
        let mut nodes = self.leaves.clone();
        let mut path = [Field::from(0); PAYNOTE_TREE_DEPTH];
        for (level, sibling) in path.iter_mut().enumerate() {
            if nodes.len() % 2 == 1 {
                nodes.push(self.zeros[level]);
            }
            *sibling = *nodes
                .get(index ^ 1)
                .ok_or_else(|| PayNoteError::InvalidInput("missing Merkle sibling".into()))?;
            nodes = nodes
                .chunks_exact(2)
                .map(|pair| merkle_node(pair[0], pair[1]))
                .collect::<Result<Vec<_>, _>>()?;
            index >>= 1;
        }
        if nodes.first() != Some(&self.root) {
            return Err(PayNoteError::InvalidInput(
                "Merkle path root mismatch".into(),
            ));
        }
        Ok((leaf_index, path))
    }
}
