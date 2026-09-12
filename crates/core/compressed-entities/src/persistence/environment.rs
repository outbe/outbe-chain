use super::{
    collection_has_records, delete_collection_records, prefixed_key, read_collection_roots,
    read_marker, read_required_tree_root, read_tree_leaf, tables,
    validate_expected_environment_identity, validate_root, ApplyOutcome, BranchKey, BranchNode,
    EnvironmentIdentity, FinalizedMarker, LeafValue, MdbxSnapshot, PersistenceError, TreeKey,
    TreeNamespace, CE_SMT_RELATIVE_PATH, IDENTITY_KEY, LAST_APPLIED_KEY,
};
use crate::sharding::aggregate_b256_shard_roots;
use crate::staging::FinalizedTreeSnapshot;
use crate::staging::ProvisionalShardBatch;
use crate::staging::StagedTreeBatch;
use crate::staging::TreeChange;
use alloy_primitives::B256;
use reth_db::database::Database;
use reth_db::mdbx::create_db;
use reth_db::mdbx::tx::Tx;
use reth_db::mdbx::DatabaseArguments;
use reth_db::mdbx::RO;
use reth_db::transaction::DbTx;
use reth_db::transaction::DbTxMut;
use reth_db::ClientVersion;
use reth_db::DatabaseEnv;
use std::path::Path;
use std::path::PathBuf;

/// Separate CE-owned MDBX environment. It does not share Reth's primary DB.
#[derive(Debug)]
pub struct CeMdbx {
    pub(super) path: PathBuf,
    pub(super) identity: EnvironmentIdentity,
    pub(super) db: DatabaseEnv,
}

fn apply_tree_changes<T: DbTxMut>(
    tx: &T,
    db: &CeMdbx,
    namespace: TreeNamespace,
    batch: &ProvisionalShardBatch,
) -> Result<(), PersistenceError> {
    apply_raw_tree_changes(
        tx,
        db,
        namespace,
        &batch.branch_changes,
        &batch.leaf_changes,
    )
}

fn apply_raw_tree_changes<T: DbTxMut>(
    tx: &T,
    db: &CeMdbx,
    namespace: TreeNamespace,
    branches: &std::collections::BTreeMap<BranchKey, TreeChange<BranchNode>>,
    leaves: &std::collections::BTreeMap<TreeKey, TreeChange<LeafValue>>,
) -> Result<(), PersistenceError> {
    for (key, change) in branches {
        let key = prefixed_key(namespace, &key.encode());
        match change {
            TreeChange::Set(node) => tx
                .put::<tables::CeBranches>(key, node.encode())
                .map_err(|error| db.db_error(error))?,
            TreeChange::Delete => {
                tx.delete::<tables::CeBranches>(key, None)
                    .map_err(|error| db.db_error(error))?;
            }
        }
    }
    for (key, change) in leaves {
        let key = prefixed_key(namespace, &key.encode());
        match change {
            TreeChange::Set(value) => tx
                .put::<tables::CeLeaves>(key, value.encode().to_vec())
                .map_err(|error| db.db_error(error))?,
            TreeChange::Delete => {
                tx.delete::<tables::CeLeaves>(key, None)
                    .map_err(|error| db.db_error(error))?;
            }
        }
    }
    Ok(())
}

impl CeMdbx {
    /// Opens `<datadir>/compressed_entities/smt/`, initializes an empty
    /// environment atomically, or verifies every existing identity field.
    pub fn open(
        datadir: &Path,
        expected_identity: EnvironmentIdentity,
        genesis_marker: FinalizedMarker,
    ) -> Result<Self, PersistenceError> {
        validate_expected_environment_identity(&expected_identity)?;
        if genesis_marker.height != 0 || genesis_marker.block_hash != expected_identity.genesis_hash
        {
            return Err(PersistenceError::InvalidGenesisMarker {
                expected_genesis_hash: expected_identity.genesis_hash,
                actual: genesis_marker,
            });
        }
        if genesis_marker.commitment_scheme_version != expected_identity.commitment_scheme_version {
            return Err(PersistenceError::EnvironmentMarkerSchemeMismatch);
        }
        validate_root(genesis_marker.parent_root)?;
        validate_root(genesis_marker.new_root)?;
        let expected_genesis_root = crate::sealed_root(B256::ZERO)
            .map_err(|error| PersistenceError::Staging(error.to_string()))?;
        if genesis_marker.new_root != expected_genesis_root {
            return Err(PersistenceError::InvalidGenesisShardRoot {
                expected: expected_genesis_root,
                actual: genesis_marker.new_root,
            });
        }

        let path = datadir.join(CE_SMT_RELATIVE_PATH);
        std::fs::create_dir_all(&path).map_err(|error| PersistenceError::Io {
            path: path.clone(),
            message: error.to_string(),
        })?;
        let args = DatabaseArguments::new(ClientVersion::default());
        let client_version = args.client_version().clone();
        let mut db = create_db(&path, args).map_err(|error| PersistenceError::Database {
            path: path.clone(),
            message: error.to_string(),
        })?;
        db.create_and_track_tables_for::<tables::CeTables>()
            .map_err(|error| PersistenceError::Database {
                path: path.clone(),
                message: error.to_string(),
            })?;
        db.record_client_version(client_version)
            .map_err(|error| PersistenceError::Database {
                path: path.clone(),
                message: error.to_string(),
            })?;

        let store = Self {
            path,
            identity: expected_identity.clone(),
            db,
        };
        store.initialize_or_verify(&expected_identity, genesis_marker)?;
        Ok(store)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn identity(&self) -> &EnvironmentIdentity {
        &self.identity
    }

    pub fn marker(&self) -> Result<FinalizedMarker, PersistenceError> {
        let tx = self.tx()?;
        let marker = read_marker(&tx, &self.path)?;
        tx.commit().map_err(|error| self.db_error(error))?;
        Ok(marker)
    }

    pub fn open_snapshot(&self) -> Result<Box<dyn FinalizedTreeSnapshot>, PersistenceError> {
        let tx = self.tx()?;
        let marker = read_marker(&tx, &self.path)?;
        let catalog_root = read_required_tree_root(&tx, &self.path, TreeNamespace::Catalog)?;
        let wrapped = crate::sealed_root(catalog_root)
            .map_err(|error| PersistenceError::Staging(error.to_string()))?;
        if wrapped != marker.new_root {
            return Err(PersistenceError::CatalogWrapperMismatch {
                expected: marker.new_root,
                actual: wrapped,
            });
        }
        Ok(Box::new(MdbxSnapshot {
            path: self.path.clone(),
            tx,
            marker,
        }))
    }

    /// Atomically applies one contiguous finalized batch and writes the marker
    /// last. A commit error is explicitly reported as an unknown outcome.
    pub fn apply_finalized(
        &self,
        batch: &StagedTreeBatch,
    ) -> Result<ApplyOutcome, PersistenceError> {
        batch
            .validate_encoded_size()
            .map_err(|error| PersistenceError::Staging(error.to_string()))?;
        validate_root(batch.parent_root())?;
        validate_root(batch.new_root())?;
        let next = batch.marker(self.identity.commitment_scheme_version);
        let tx = self.db.tx_mut().map_err(|error| self.db_error(error))?;
        let current = read_marker(&tx, &self.path)?;

        if next == current {
            tx.commit().map_err(|error| self.db_error(error))?;
            return Ok(ApplyOutcome::AlreadyApplied(current));
        }
        if next.height <= current.height {
            return Err(PersistenceError::ConflictingFinalizedMarker { current, next });
        }
        if next.height != current.height.saturating_add(1)
            || next.parent_block_hash != current.block_hash
            || next.parent_root != current.new_root
            || next.commitment_scheme_version != current.commitment_scheme_version
        {
            return Err(PersistenceError::NonContiguousFinalizedApply { current, next });
        }

        let parent_catalog = read_required_tree_root(&tx, &self.path, TreeNamespace::Catalog)?;
        if parent_catalog != batch.parent_catalog_root
            || crate::sealed_root(parent_catalog)
                .map_err(|error| PersistenceError::Staging(error.to_string()))?
                != current.new_root
        {
            return Err(PersistenceError::ParentCatalogRootMismatch);
        }

        for (collection_key, operation) in &batch.changed_collections {
            let catalog_key = TreeKey::try_from(B256::from(*collection_key.as_bytes()))?;
            let catalog_leaf =
                read_tree_leaf(&tx, &self.path, TreeNamespace::Catalog, catalog_key)?
                    .map(LeafValue::into_inner);
            match operation {
                crate::CollectionOperation::Mutate(collection) => {
                    let domain = crate::CeDomain::try_from(collection.domain_id)
                        .map_err(|_| PersistenceError::InvalidTopologyIdentity)?;
                    let shard_count = domain.shard_count();
                    if catalog_leaf != collection.parent_collection_root {
                        return Err(PersistenceError::ParentCatalogRootMismatch);
                    }
                    let persisted =
                        read_collection_roots(&tx, &self.path, *collection_key, shard_count)?;
                    match (&persisted, collection.parent_collection_root) {
                        (None, None) => {
                            if collection_has_records(&tx, &self.path, *collection_key)? {
                                return Err(PersistenceError::OrphanCollectionRecords {
                                    collection: *collection_key,
                                });
                            }
                        }
                        (Some(roots), Some(_))
                            if roots == &collection.shard_set.parent_shard_roots => {}
                        _ => return Err(PersistenceError::ParentShardRootsMismatch),
                    }

                    for shard_index in 0..shard_count {
                        let namespace =
                            TreeNamespace::CollectionShard(*collection_key, shard_index);
                        let position = usize::try_from(shard_index).map_err(|_| {
                            PersistenceError::InvalidShardCount {
                                actual: shard_index,
                            }
                        })?;
                        if let Some(shard) = collection.shard_set.changed_shards.get(&shard_index) {
                            apply_tree_changes(&tx, self, namespace, shard)?;
                        }
                        tx.put::<tables::CeTreeRoots>(
                            namespace.encode(),
                            collection.shard_set.new_shard_roots[position]
                                .as_slice()
                                .to_vec(),
                        )
                        .map_err(|error| self.db_error(error))?;
                    }
                    let top = aggregate_b256_shard_roots(&collection.shard_set.new_shard_roots)
                        .map_err(|error| PersistenceError::Staging(error.to_string()))?;
                    let recomputed = crate::collection_root(domain, *collection_key, top)
                        .map_err(|error| PersistenceError::Staging(error.to_string()))?;
                    if recomputed != collection.new_collection_root {
                        return Err(PersistenceError::NewCollectionRootMismatch);
                    }
                }
                crate::CollectionOperation::Retire(retirement) => {
                    let domain = crate::CeDomain::try_from(retirement.domain_id)
                        .map_err(|_| PersistenceError::InvalidTopologyIdentity)?;
                    if domain != crate::CeDomain::Tribute
                        || catalog_leaf != Some(retirement.parent_collection_root)
                    {
                        return Err(PersistenceError::ParentCatalogRootMismatch);
                    }
                    let persisted = read_collection_roots(
                        &tx,
                        &self.path,
                        *collection_key,
                        domain.shard_count(),
                    )?;
                    if persisted.as_ref() != Some(&retirement.parent_shard_roots) {
                        return Err(PersistenceError::ParentShardRootsMismatch);
                    }
                    let top = aggregate_b256_shard_roots(&retirement.parent_shard_roots)
                        .map_err(|error| PersistenceError::Staging(error.to_string()))?;
                    if crate::collection_root(domain, *collection_key, top)
                        .map_err(|error| PersistenceError::Staging(error.to_string()))?
                        != retirement.parent_collection_root
                    {
                        return Err(PersistenceError::ParentCatalogRootMismatch);
                    }
                    delete_collection_records(&tx, self, *collection_key)?;
                }
            }
        }

        if let Some(catalog) = &batch.catalog_batch {
            apply_raw_tree_changes(
                &tx,
                self,
                TreeNamespace::Catalog,
                &catalog.branch_changes,
                &catalog.leaf_changes,
            )?;
            tx.put::<tables::CeTreeRoots>(
                TreeNamespace::Catalog.encode(),
                batch.new_catalog_root.as_slice().to_vec(),
            )
            .map_err(|error| self.db_error(error))?;
        }
        let wrapped = crate::sealed_root(batch.new_catalog_root)
            .map_err(|error| PersistenceError::Staging(error.to_string()))?;
        if wrapped != batch.new_root() {
            return Err(PersistenceError::CatalogWrapperMismatch {
                expected: batch.new_root(),
                actual: wrapped,
            });
        }
        // Progress is deliberately the final write in this transaction.
        tx.put::<tables::CeMetadata>(LAST_APPLIED_KEY.to_vec(), next.encode().to_vec())
            .map_err(|error| self.db_error(error))?;
        tx.commit()
            .map_err(|error| PersistenceError::CommitOutcomeUnknown {
                path: self.path.clone(),
                marker: next,
                message: error.to_string(),
            })?;
        Ok(ApplyOutcome::Applied(next))
    }

    fn initialize_or_verify(
        &self,
        expected_identity: &EnvironmentIdentity,
        genesis_marker: FinalizedMarker,
    ) -> Result<(), PersistenceError> {
        let tx = self.db.tx().map_err(|error| self.db_error(error))?;
        let stored_identity = tx
            .get::<tables::CeMetadata>(IDENTITY_KEY.to_vec())
            .map_err(|error| self.db_error(error))?;
        let stored_marker = tx
            .get::<tables::CeMetadata>(LAST_APPLIED_KEY.to_vec())
            .map_err(|error| self.db_error(error))?;
        let branch_records = tx
            .entries::<tables::CeBranches>()
            .map_err(|error| self.db_error(error))?;
        let leaf_records = tx
            .entries::<tables::CeLeaves>()
            .map_err(|error| self.db_error(error))?;
        let shard_root_records = tx
            .entries::<tables::CeTreeRoots>()
            .map_err(|error| self.db_error(error))?;
        tx.commit().map_err(|error| self.db_error(error))?;

        match (stored_identity, stored_marker) {
            (None, None) => {
                if branch_records != 0 || leaf_records != 0 || shard_root_records != 0 {
                    return Err(PersistenceError::OrphanTreeRecords {
                        branches: branch_records,
                        leaves: leaf_records,
                        shard_roots: shard_root_records,
                    });
                }
                let identity = expected_identity.encode()?;
                let tx = self.db.tx_mut().map_err(|error| self.db_error(error))?;
                tx.put::<tables::CeMetadata>(IDENTITY_KEY.to_vec(), identity)
                    .map_err(|error| self.db_error(error))?;
                tx.put::<tables::CeTreeRoots>(
                    TreeNamespace::Catalog.encode(),
                    B256::ZERO.as_slice().to_vec(),
                )
                .map_err(|error| self.db_error(error))?;
                tx.put::<tables::CeMetadata>(
                    LAST_APPLIED_KEY.to_vec(),
                    genesis_marker.encode().to_vec(),
                )
                .map_err(|error| self.db_error(error))?;
                tx.commit()
                    .map_err(|error| PersistenceError::CommitOutcomeUnknown {
                        path: self.path.clone(),
                        marker: genesis_marker,
                        message: error.to_string(),
                    })?;
            }
            (Some(identity), Some(marker)) => {
                let actual_identity = EnvironmentIdentity::decode(&identity)?;
                if &actual_identity != expected_identity {
                    return Err(PersistenceError::EnvironmentIdentityMismatch {
                        expected: expected_identity.clone(),
                        actual: actual_identity,
                    });
                }
                let marker = FinalizedMarker::decode(&marker)?;
                if marker.commitment_scheme_version != expected_identity.commitment_scheme_version {
                    return Err(PersistenceError::EnvironmentMarkerSchemeMismatch);
                }
                let tx = self.tx()?;
                let catalog_root =
                    read_required_tree_root(&tx, &self.path, TreeNamespace::Catalog)?;
                let wrapped = crate::sealed_root(catalog_root)
                    .map_err(|error| PersistenceError::Staging(error.to_string()))?;
                if wrapped != marker.new_root {
                    return Err(PersistenceError::CatalogWrapperMismatch {
                        expected: marker.new_root,
                        actual: wrapped,
                    });
                }
                tx.commit().map_err(|error| self.db_error(error))?;
            }
            _ => return Err(PersistenceError::PartialEnvironmentInitialization),
        }
        Ok(())
    }

    fn tx(&self) -> Result<Tx<RO>, PersistenceError> {
        self.db.tx().map_err(|error| self.db_error(error))
    }

    pub(super) fn db_error(&self, error: impl std::fmt::Display) -> PersistenceError {
        PersistenceError::Database {
            path: self.path.clone(),
            message: error.to_string(),
        }
    }
}
