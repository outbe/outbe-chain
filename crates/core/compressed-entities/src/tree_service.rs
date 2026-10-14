mod seal;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use alloy_primitives::B256;
use outbe_primitives::error::{PrecompileError, Result};

use crate::{
    api::{AuthenticatedParentTree, EntityRef, FinalLeafMutation},
    collection::{collection_key, collection_root, sealed_root},
    persistence::{CeMdbx, ExactParentIdentity, PersistenceError, TreeNamespace},
    schema::Collection,
    sharding::{aggregate_b256_shard_roots, shard_index},
    smt::{derive_tree_key, PoseidonSmt, TreeKey, TreeLeaf, TreeRoot},
    staging::{
        AuthenticatedCatalogView, CollectionBatch, ProvisionalCatalogBatch,
        ProvisionalShardSetBatch, ShardIndex, StagingCkbStore, StagingError,
    },
    CeDomain, CollectionKey, Commitment, ProvisionalTreeBatch,
};

type CollectionMutation = (EntityRef, TreeKey, TreeLeaf);
type GroupedCollectionMutations = BTreeMap<CollectionKey, (CeDomain, Vec<CollectionMutation>)>;

struct CollectionSession {
    domain: CeDomain,
    parent_collection_root: Option<B256>,
    parent_shard_roots: Vec<B256>,
    trees: Option<Vec<PoseidonSmt<StagingCkbStore>>>,
}

struct SessionState {
    catalog_tree: Option<PoseidonSmt<StagingCkbStore>>,
    collections: BTreeMap<CollectionKey, CollectionSession>,
    verified_leaves: BTreeMap<EntityRef, Option<Commitment>>,
}

/// One exact-finalized-parent catalog session for block execution.
///
/// The Root Catalog is opened eagerly. Collection shard sets are opened only
/// after their catalog leaf is verified against the same MDBX snapshot.
pub struct MdbxAuthenticatedTree {
    identity: ExactParentIdentity,
    view: AuthenticatedCatalogView,
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
        let view =
            AuthenticatedCatalogView::open(snapshot, identity).map_err(classify_staging_error)?;
        Self::from_view(view)
    }

    pub(crate) fn from_view(view: AuthenticatedCatalogView) -> Result<Self> {
        let identity = view.identity();
        let catalog_root =
            TreeRoot::from_be_bytes(view.catalog_root().0).map_err(tree_corruption)?;
        let catalog_store =
            StagingCkbStore::new(view.clone(), TreeNamespace::Catalog, view.catalog_root());
        Ok(Self {
            identity,
            view,
            state: Mutex::new(SessionState {
                catalog_tree: Some(PoseidonSmt::open_with_store(catalog_root, catalog_store)),
                collections: BTreeMap::new(),
                verified_leaves: BTreeMap::new(),
            }),
        })
    }

    fn ensure_collection(
        &self,
        state: &mut SessionState,
        domain: CeDomain,
        key: CollectionKey,
    ) -> Result<()> {
        if let Some(existing) = state.collections.get(&key) {
            if existing.domain != domain {
                return Err(tree_corruption("collection key/domain collision"));
            }
            return Ok(());
        }
        self.load_collection(state, domain, key)
    }
    fn load_collection(
        &self,
        state: &mut SessionState,
        domain: CeDomain,
        key: CollectionKey,
    ) -> Result<()> {
        let catalog = state
            .catalog_tree
            .as_ref()
            .ok_or_else(|| tree_corruption("authenticated catalog session already sealed"))?;
        let catalog_key = TreeKey::from_be_bytes(*key.as_bytes()).map_err(tree_corruption)?;
        let leaf = catalog.get(catalog_key).map_err(tree_corruption)?;
        let proof = catalog.prove(vec![catalog_key]).map_err(tree_corruption)?;
        catalog
            .verify(
                TreeRoot::from_be_bytes(self.view.catalog_root().0).map_err(tree_corruption)?,
                &proof,
                vec![(catalog_key, leaf)],
            )
            .map_err(tree_corruption)?;

        if leaf == TreeLeaf::ZERO {
            if self
                .view
                .collection_has_records(key)
                .map_err(tree_corruption)?
            {
                return Err(tree_corruption(
                    "orphan collection records behind catalog non-membership",
                ));
            }
            state.collections.insert(
                key,
                CollectionSession {
                    domain,
                    parent_collection_root: None,
                    parent_shard_roots: vec![B256::ZERO; domain.shard_count() as usize],
                    trees: None,
                },
            );
            return Ok(());
        }

        let parent_collection_root = B256::from(leaf.as_bytes());
        let root_count = self
            .view
            .collection_root_count(key)
            .map_err(tree_corruption)?;
        if root_count != domain.shard_count() as usize {
            return Err(tree_corruption(format!(
                "present collection has {root_count} shard roots, expected {}",
                domain.shard_count()
            )));
        }
        let mut roots = Vec::with_capacity(domain.shard_count() as usize);
        let mut trees = Vec::with_capacity(domain.shard_count() as usize);
        for shard in 0..domain.shard_count() {
            let namespace = TreeNamespace::CollectionShard(key, shard);
            let root = self
                .view
                .tree_root(namespace)
                .map_err(tree_corruption)?
                .ok_or_else(|| tree_corruption("present collection has a missing shard root"))?;
            let tree_root = TreeRoot::from_be_bytes(root.0).map_err(tree_corruption)?;
            trees.push(PoseidonSmt::open_with_store(
                tree_root,
                StagingCkbStore::new(self.view.clone(), namespace, root),
            ));
            roots.push(root);
        }
        let top = aggregate_b256_shard_roots(&roots).map_err(tree_corruption)?;
        let recomputed = collection_root(domain, key, top).map_err(tree_corruption)?;
        if recomputed != parent_collection_root {
            return Err(tree_corruption(
                "catalog collection root does not match exact shard-root vector",
            ));
        }
        state.collections.insert(
            key,
            CollectionSession {
                domain,
                parent_collection_root: Some(parent_collection_root),
                parent_shard_roots: roots,
                trees: Some(trees),
            },
        );
        Ok(())
    }

    fn ensure_mutable_trees(&self, key: CollectionKey, session: &mut CollectionSession) {
        if session.trees.is_some() {
            return;
        }
        let trees = (0..session.domain.shard_count())
            .map(|shard| {
                PoseidonSmt::open_with_store(
                    TreeRoot::EMPTY,
                    StagingCkbStore::new(
                        self.view.clone(),
                        TreeNamespace::CollectionShard(key, shard),
                        B256::ZERO,
                    ),
                )
            })
            .collect();
        session.trees = Some(trees);
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
                "requested EVM root does not match exact catalog view",
            ));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| tree_corruption("authenticated catalog session lock poisoned"))?;
        if let Some(cached) = state.verified_leaves.get(&entity) {
            return Ok(*cached);
        }
        let (domain, _, id) = entity_parts(entity);
        let key = collection_key(domain, id).map_err(tree_corruption)?;
        self.ensure_collection(&mut state, domain, key)?;
        let session = state
            .collections
            .get(&key)
            .ok_or_else(|| tree_corruption("verified collection cache entry is missing"))?;
        let Some(trees) = session.trees.as_ref() else {
            state.verified_leaves.insert(entity, None);
            return Ok(None);
        };
        let tree_key = entity_tree_key(entity)?;
        let shard = shard_index(tree_key, domain.shard_count()).map_err(tree_corruption)?;
        let tree = &trees[shard as usize];
        let leaf = tree.get(tree_key).map_err(tree_corruption)?;
        let proof = tree.prove(vec![tree_key]).map_err(tree_corruption)?;
        tree.verify(
            TreeRoot::from_be_bytes(session.parent_shard_roots[shard as usize].0)
                .map_err(tree_corruption)?,
            &proof,
            vec![(tree_key, leaf)],
        )
        .map_err(tree_corruption)?;
        let commitment = if leaf == TreeLeaf::ZERO {
            None
        } else {
            Some(Commitment::try_from(leaf.as_bytes()).map_err(tree_corruption)?)
        };
        state.verified_leaves.insert(entity, commitment);
        Ok(commitment)
    }

    fn partition_present_verified(
        &self,
        partition: crate::PartitionRef,
        expected_parent_root: B256,
    ) -> Result<bool> {
        if expected_parent_root != self.identity.root {
            return Err(tree_corruption(
                "requested EVM root does not match exact catalog view",
            ));
        }
        let (domain, key) = crate::partition_collection_key(partition).map_err(tree_corruption)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| tree_corruption("authenticated catalog session lock poisoned"))?;
        self.ensure_collection(&mut state, domain, key)?;
        Ok(state
            .collections
            .get(&key)
            .and_then(|collection| collection.parent_collection_root)
            .is_some())
    }

    fn partition_root_verified(
        &self,
        partition: crate::PartitionRef,
        expected_parent_root: B256,
    ) -> Result<Option<B256>> {
        if expected_parent_root != self.identity.root {
            return Err(tree_corruption(
                "requested EVM root does not match exact catalog view",
            ));
        }
        let (domain, key) = crate::partition_collection_key(partition).map_err(tree_corruption)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| tree_corruption("authenticated catalog session lock poisoned"))?;
        if !state.collections.contains_key(&key) {
            if state.catalog_tree.is_some() {
                self.ensure_collection(&mut state, domain, key)?;
            } else {
                let catalog_root =
                    TreeRoot::from_be_bytes(self.view.catalog_root().0).map_err(tree_corruption)?;
                let catalog_store = StagingCkbStore::new(
                    self.view.clone(),
                    TreeNamespace::Catalog,
                    self.view.catalog_root(),
                );
                let mut verification = SessionState {
                    catalog_tree: Some(PoseidonSmt::open_with_store(catalog_root, catalog_store)),
                    collections: BTreeMap::new(),
                    verified_leaves: BTreeMap::new(),
                };
                self.ensure_collection(&mut verification, domain, key)?;
                let collection = verification
                    .collections
                    .remove(&key)
                    .ok_or_else(|| tree_corruption("verified collection cache entry is missing"))?;
                state.collections.insert(key, collection);
            }
        }
        Ok(state
            .collections
            .get(&key)
            .and_then(|collection| collection.parent_collection_root))
    }

    fn prepare_seal(
        &self,
        block_number: u64,
        mutations: &[FinalLeafMutation],
        retirements: &[crate::PartitionRef],
    ) -> Result<ProvisionalTreeBatch> {
        self.prepare_batch(block_number, mutations, retirements)
    }
}

fn entity_parts(entity: EntityRef) -> (CeDomain, Collection, crate::WwdEntityId) {
    match entity {
        EntityRef::Tribute(id) => (CeDomain::Tribute, Collection::Tribute, id),
        EntityRef::NodItem(id) => (CeDomain::NodItem, Collection::NodItem, id),
        EntityRef::NodBucket(id) => (CeDomain::NodBucket, Collection::NodBucket, id),
        EntityRef::Gem(id) => (CeDomain::Gem, Collection::Gem, id),
        EntityRef::GemBucket(id) => (CeDomain::GemBucket, Collection::GemBucket, id),
    }
}

fn entity_tree_key(entity: EntityRef) -> Result<TreeKey> {
    let (_, collection, id) = entity_parts(entity);
    derive_tree_key(collection, id).map_err(tree_corruption)
}

fn tree_unavailable(message: impl Into<String>) -> PrecompileError {
    PrecompileError::TreeUnavailable(message.into())
}

fn classify_snapshot_error(error: PersistenceError) -> PrecompileError {
    match error {
        PersistenceError::Io { .. } | PersistenceError::Database { .. } => {
            tree_unavailable(error.to_string())
        }
        PersistenceError::ExactParentMismatch { required, actual }
            if required.commitment_scheme_version == actual.commitment_scheme_version
                && required.block_number != actual.height =>
        {
            // Payload jobs are asynchronous. An old job can legitimately run
            // after finalization advanced the in-place materialization. A
            // catching-up node can request a parent ahead of its marker. Neither
            // height skew proves corruption. Same-height hash/root mismatches and
            // scheme mismatches remain fatal below.
            tree_unavailable(PersistenceError::ExactParentMismatch { required, actual }.to_string())
        }
        corruption => tree_corruption(corruption),
    }
}

fn classify_staging_error(error: StagingError) -> PrecompileError {
    match error {
        StagingError::Persistence(error) => classify_snapshot_error(error),
        corruption => tree_corruption(corruption),
    }
}

fn tree_corruption(message: impl core::fmt::Display) -> PrecompileError {
    crate::errors::tree_corruption(message)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{FinalizedMarker, ACTIVE_COMMITMENT_SCHEME};

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

        let required = ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 120,
            block_hash: B256::repeat_byte(0x12),
            root: B256::repeat_byte(0x34),
        };
        let finalized_ahead = FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: 121,
            block_hash: B256::repeat_byte(0x56),
            parent_block_hash: required.block_hash,
            parent_root: required.root,
            new_root: required.root,
        };
        assert!(matches!(
            classify_snapshot_error(PersistenceError::ExactParentMismatch {
                required,
                actual: finalized_ahead,
            }),
            PrecompileError::TreeUnavailable(_)
        ));
        assert!(matches!(
            classify_staging_error(StagingError::Persistence(
                PersistenceError::ExactParentMismatch {
                    required,
                    actual: finalized_ahead,
                },
            )),
            PrecompileError::TreeUnavailable(_)
        ));

        let wrong_same_height = FinalizedMarker {
            height: required.block_number,
            ..finalized_ahead
        };
        assert!(matches!(
            classify_snapshot_error(PersistenceError::ExactParentMismatch {
                required,
                actual: wrong_same_height,
            }),
            PrecompileError::Fatal(_)
        ));
    }
}
