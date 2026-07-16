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
    sharding::{aggregate_b256_shard_roots, shard_index},
    smt::{derive_tree_key, PoseidonSmt, TreeKey, TreeLeaf, TreeRoot},
    staging::{AuthenticatedTreeView, ProvisionalShardBatch, ShardIndex, StagingCkbStore},
    Commitment, ProvisionalTreeBatch,
};

struct SessionState {
    trees: Option<Vec<PoseidonSmt<StagingCkbStore>>>,
    verified_leaves: BTreeMap<EntityRef, Option<Commitment>>,
}

/// One exact-finalized-parent CKB tree session for block execution.
///
/// The session owns one immutable MDBX read snapshot. Point reads verify CKB
/// evidence before returning a leaf, and end-block sealing consumes the same
/// snapshot into an isolated staged store without an MDBX write transaction.
pub struct MdbxAuthenticatedTree {
    identity: ExactParentIdentity,
    shard_count: u32,
    parent_shard_roots: Vec<B256>,
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
        let shard_count = db.identity().shard_count;
        let view = AuthenticatedTreeView::open(snapshot, identity, shard_count)
            .map_err(|error| tree_corruption(error.to_string()))?;
        let parent_shard_roots = view.shard_roots().to_vec();
        let mut trees = Vec::with_capacity(parent_shard_roots.len());
        for (index, root) in parent_shard_roots.iter().copied().enumerate() {
            let shard_index = u32::try_from(index)
                .map_err(|_| tree_corruption("shard index is not representable"))?;
            let root = TreeRoot::from_be_bytes(root.0)
                .map_err(|error| tree_corruption(error.to_string()))?;
            let store = StagingCkbStore::new(view.clone(), shard_index)
                .map_err(|error| tree_corruption(error.to_string()))?;
            trees.push(PoseidonSmt::open_with_store(root, store));
        }
        Ok(Self {
            identity,
            shard_count,
            parent_shard_roots,
            state: Mutex::new(SessionState {
                trees: Some(trees),
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
        let shard = shard_index(key, self.shard_count)
            .map_err(|error| tree_corruption(error.to_string()))?;
        let tree = state
            .trees
            .as_ref()
            .ok_or_else(|| tree_corruption("authenticated tree session already sealed"))?
            .get(usize::try_from(shard).map_err(|_| tree_corruption("shard index overflow"))?)
            .ok_or_else(|| tree_corruption("derived shard index out of range"))?;
        let leaf = tree
            .get(key)
            .map_err(|error| tree_corruption(error.to_string()))?;
        let proof = tree
            .prove(vec![key])
            .map_err(|error| tree_corruption(error.to_string()))?;
        tree.verify(
            TreeRoot::from_be_bytes(
                self.parent_shard_roots[usize::try_from(shard)
                    .map_err(|_| tree_corruption("shard index overflow"))?]
                .0,
            )
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
        let trees = state
            .trees
            .take()
            .ok_or_else(|| tree_corruption("compressed-entity tree sealed more than once"))?;

        let result = (|| {
            let mut keyed_by_shard: BTreeMap<ShardIndex, Vec<(TreeKey, TreeLeaf)>> =
                BTreeMap::new();
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
                let shard = shard_index(key, self.shard_count)
                    .map_err(|error| tree_corruption(error.to_string()))?;
                keyed_by_shard
                    .entry(shard)
                    .or_default()
                    .push((key, final_leaf));
            }
            for keyed in keyed_by_shard.values_mut() {
                keyed.sort_by_key(|(key, _)| *key);
            }

            let mut new_shard_roots = self.parent_shard_roots.clone();
            let mut changed_shards: BTreeMap<ShardIndex, ProvisionalShardBatch> = BTreeMap::new();
            for (index, mut tree) in trees.into_iter().enumerate() {
                let shard = u32::try_from(index)
                    .map_err(|_| tree_corruption("shard index is not representable"))?;
                let Some(keyed) = keyed_by_shard.remove(&shard) else {
                    continue;
                };
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
                    TreeRoot::from_be_bytes(self.parent_shard_roots[index].0)
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
                let new_shard_root = B256::from(
                    tree.root()
                        .map_err(|error| tree_corruption(error.to_string()))?
                        .as_bytes(),
                );
                new_shard_roots[index] = new_shard_root;
                if let Some(batch) = tree
                    .into_store()
                    .freeze_shard(new_shard_root)
                    .map_err(|error| tree_corruption(error.to_string()))?
                {
                    changed_shards.insert(shard, batch);
                }
            }
            if !keyed_by_shard.is_empty() {
                return Err(tree_corruption("derived mutation shard was not prepared"));
            }
            let new_root = if mutations.is_empty() {
                self.identity.root
            } else {
                aggregate_b256_shard_roots(&new_shard_roots)
                    .map_err(|error| tree_corruption(error.to_string()))?
            };
            ProvisionalTreeBatch::new(
                block_number,
                self.identity.block_hash,
                self.identity.root,
                new_root,
                self.shard_count,
                self.parent_shard_roots.clone(),
                new_shard_roots,
                changed_shards,
            )
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
