use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use alloy_primitives::B256;
use outbe_primitives::error::{PrecompileError, Result};

use crate::{
    api::{AuthenticatedParentTree, EntityRef, FinalLeafMutation},
    persistence::{CeMdbx, ExactParentIdentity, PersistenceError},
    schema::Collection,
    smt::{derive_tree_key, PoseidonSmt, TreeKey, TreeLeaf, TreeRoot},
    staging::{AuthenticatedTreeView, StagingCkbStore},
    Commitment, ProvisionalTreeBatch,
};

struct SessionState {
    tree: Option<PoseidonSmt<StagingCkbStore>>,
    verified_leaves: BTreeMap<EntityRef, Option<Commitment>>,
}

/// One exact-finalized-parent CKB tree session for block execution.
///
/// The session owns one immutable MDBX read snapshot. Point reads verify CKB
/// evidence before returning a leaf, and end-block sealing consumes the same
/// snapshot into an isolated staged store without an MDBX write transaction.
pub struct MdbxAuthenticatedTree {
    identity: ExactParentIdentity,
    state: Mutex<SessionState>,
}

impl core::fmt::Debug for MdbxAuthenticatedTree {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("MdbxAuthenticatedTree")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl MdbxAuthenticatedTree {
    pub fn open(db: Arc<CeMdbx>, identity: ExactParentIdentity) -> Result<Self> {
        let snapshot = db.open_snapshot().map_err(classify_snapshot_error)?;
        let view = AuthenticatedTreeView::open(snapshot, identity)
            .map_err(|error| tree_corruption(error.to_string()))?;
        let root = TreeRoot::from_be_bytes(identity.root.0)
            .map_err(|error| tree_corruption(error.to_string()))?;
        let store = StagingCkbStore::new(view);
        let tree = PoseidonSmt::open_with_store(root, store);
        Ok(Self {
            identity,
            state: Mutex::new(SessionState {
                tree: Some(tree),
                verified_leaves: BTreeMap::new(),
            }),
        })
    }
}

impl AuthenticatedParentTree for MdbxAuthenticatedTree {
    fn parent_block_hash(&self) -> B256 {
        self.identity.block_hash
    }

    fn parent_root(&self) -> B256 {
        self.identity.root
    }

    fn read_leaf_verified(
        &self,
        entity: EntityRef,
        expected_parent_root: B256,
    ) -> Result<Option<Commitment>> {
        if expected_parent_root != self.identity.root {
            return Err(tree_corruption(
                "requested EVM root does not match exact tree view",
            ));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| tree_corruption("authenticated tree session lock poisoned"))?;
        if let Some(cached) = state.verified_leaves.get(&entity) {
            return Ok(*cached);
        }
        let key = entity_tree_key(entity)?;
        let tree = state
            .tree
            .as_ref()
            .ok_or_else(|| tree_corruption("authenticated tree session already sealed"))?;
        let leaf = tree
            .get(key)
            .map_err(|error| tree_corruption(error.to_string()))?;
        let proof = tree
            .prove(vec![key])
            .map_err(|error| tree_corruption(error.to_string()))?;
        tree.verify(
            TreeRoot::from_be_bytes(expected_parent_root.0)
                .map_err(|error| tree_corruption(error.to_string()))?,
            &proof,
            vec![(key, leaf)],
        )
        .map_err(|error| tree_corruption(error.to_string()))?;
        let commitment = if leaf == TreeLeaf::ZERO {
            None
        } else {
            Some(
                Commitment::try_from(leaf.as_bytes())
                    .map_err(|error| tree_corruption(error.to_string()))?,
            )
        };
        state.verified_leaves.insert(entity, commitment);
        Ok(commitment)
    }

    fn prepare_seal(
        &self,
        block_number: u64,
        mutations: &[FinalLeafMutation],
    ) -> Result<ProvisionalTreeBatch> {
        if block_number != self.identity.block_number.saturating_add(1) {
            return Err(tree_corruption(
                "compressed-entity seal block is not parent height + 1",
            ));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| tree_corruption("authenticated tree session lock poisoned"))?;
        let mut tree = state
            .tree
            .take()
            .ok_or_else(|| tree_corruption("compressed-entity tree sealed more than once"))?;

        let result = (|| {
            let mut keyed = Vec::with_capacity(mutations.len());
            let mut unique_entities = BTreeSet::new();
            let mut unique_keys = BTreeMap::new();
            for mutation in mutations {
                if !unique_entities.insert(mutation.entity) {
                    return Err(tree_corruption(
                        "duplicate compressed-entity identity at seal",
                    ));
                }
                let key = entity_tree_key(mutation.entity)?;
                if let Some(other) = unique_keys.insert(key, mutation.entity) {
                    if other != mutation.entity {
                        return Err(tree_corruption("compressed-entity tree-key collision"));
                    }
                }
                let final_leaf = mutation
                    .final_leaf
                    .map_or(Ok(TreeLeaf::ZERO), |commitment| {
                        TreeLeaf::from_be_bytes(*commitment.as_bytes())
                            .map_err(|error| tree_corruption(error.to_string()))
                    })?;
                keyed.push((key, final_leaf));
            }
            keyed.sort_by_key(|(key, _)| *key);

            if !keyed.is_empty() {
                let keys = keyed.iter().map(|(key, _)| *key).collect::<Vec<_>>();
                let parent_leaves = keys
                    .iter()
                    .map(|key| {
                        tree.get(*key)
                            .map(|leaf| (*key, leaf))
                            .map_err(|error| tree_corruption(error.to_string()))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let proof = tree
                    .prove(keys)
                    .map_err(|error| tree_corruption(error.to_string()))?;
                tree.verify(
                    TreeRoot::from_be_bytes(self.identity.root.0)
                        .map_err(|error| tree_corruption(error.to_string()))?,
                    &proof,
                    parent_leaves.clone(),
                )
                .map_err(|error| tree_corruption(error.to_string()))?;

                let effective = keyed
                    .into_iter()
                    .zip(parent_leaves)
                    .filter_map(|((key, final_leaf), (parent_key, parent_leaf))| {
                        (key == parent_key && final_leaf != parent_leaf)
                            .then_some((key, final_leaf))
                    })
                    .collect::<Vec<_>>();
                if !effective.is_empty() {
                    tree.update_all(effective)
                        .map_err(|error| tree_corruption(error.to_string()))?;
                }
            }

            let new_root = B256::from(
                tree.root()
                    .map_err(|error| tree_corruption(error.to_string()))?
                    .as_bytes(),
            );
            tree.into_store()
                .freeze(block_number, self.identity.block_hash, new_root)
                .map_err(|error| tree_corruption(error.to_string()))
        })();

        if result.is_err() {
            // A failed seal is terminal for this block-scoped session. Keeping
            // it consumed prevents accidental retry against partial staging.
            state.verified_leaves.clear();
        }
        result
    }
}

fn entity_tree_key(entity: EntityRef) -> Result<TreeKey> {
    let (collection, id) = match entity {
        EntityRef::Tribute(id) => (Collection::Tribute, id),
        EntityRef::NodItem(id) => (Collection::NodItem, id),
        EntityRef::NodBucket(id) => (Collection::NodBucket, id),
    };
    derive_tree_key(collection, id).map_err(|error| tree_corruption(error.to_string()))
}

fn tree_unavailable(message: impl Into<String>) -> PrecompileError {
    PrecompileError::TreeUnavailable(message.into())
}

fn classify_snapshot_error(error: PersistenceError) -> PrecompileError {
    match error {
        PersistenceError::Io { .. } | PersistenceError::Database { .. } => {
            tree_unavailable(error.to_string())
        }
        corruption => tree_corruption(corruption.to_string()),
    }
}

fn tree_corruption(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(format!(
        "compressed-entity tree corruption: {}",
        message.into()
    ))
}

#[cfg(test)]
mod classification_tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn only_technical_snapshot_failures_are_retryable_readiness() {
        assert!(matches!(
            classify_snapshot_error(PersistenceError::Database {
                path: PathBuf::from("ce.mdbx"),
                message: "reader unavailable".to_owned(),
            }),
            PrecompileError::TreeUnavailable(_)
        ));
        assert!(matches!(
            classify_snapshot_error(PersistenceError::MalformedCodec {
                record: "last_applied",
                expected: "140",
                actual: 7,
            }),
            PrecompileError::Fatal(_)
        ));
        assert!(matches!(
            classify_snapshot_error(PersistenceError::HashPoison),
            PrecompileError::Fatal(_)
        ));
    }
}
