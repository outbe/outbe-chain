//! Apply finalized batches atomically, with progress as the final write.

use super::*;
use crate::{
    staging::{CollectionBatch, ProvisionalShardSetBatch, RetirementBatch},
    CollectionKey,
};

impl CeMdbx {
    /// Atomically applies one contiguous finalized batch and writes the marker
    /// last. It explicitly reports a commit error as an unknown outcome.
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
            self.apply_collection(&tx, *collection_key, operation)?;
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
    fn apply_collection(
        &self,
        tx: &(impl DbTxMut + DbTx),
        collection_key: CollectionKey,
        operation: &crate::CollectionOperation,
    ) -> Result<(), PersistenceError> {
        let catalog_key = TreeKey::try_from(B256::from(collection_key.as_bytes()))?;
        let catalog_leaf = read_tree_leaf(tx, &self.path, TreeNamespace::Catalog, catalog_key)?
            .map(LeafValue::into_inner);
        match operation {
            crate::CollectionOperation::Mutate(collection) => {
                self.apply_mutation(tx, collection_key, collection, catalog_leaf)?
            }
            crate::CollectionOperation::Retire(retirement) => {
                self.apply_retirement(tx, collection_key, retirement, catalog_leaf)?
            }
        }
        Ok(())
    }
    fn apply_mutation(
        &self,
        tx: &(impl DbTxMut + DbTx),
        collection_key: CollectionKey,
        collection: &CollectionBatch,
        catalog_leaf: Option<B256>,
    ) -> Result<(), PersistenceError> {
        let domain = crate::CeDomain::try_from(collection.domain_id)
            .map_err(|_| PersistenceError::InvalidTopologyIdentity)?;
        let shard_count = domain.shard_count();
        if catalog_leaf != collection.parent_collection_root {
            return Err(PersistenceError::ParentCatalogRootMismatch);
        }
        let persisted = read_collection_roots(tx, &self.path, collection_key, shard_count)?;
        match (&persisted, collection.parent_collection_root) {
            (None, None) => {
                if collection_has_records(tx, &self.path, collection_key)? {
                    return Err(PersistenceError::OrphanCollectionRecords {
                        collection: collection_key,
                    });
                }
            }
            (Some(roots), Some(_)) if roots == &collection.shard_set.parent_shard_roots => {}
            _ => return Err(PersistenceError::ParentShardRootsMismatch),
        }

        self.apply_collection_shards(tx, collection_key, &collection.shard_set)?;
        let top = aggregate_b256_shard_roots(&collection.shard_set.new_shard_roots)
            .map_err(|error| PersistenceError::Staging(error.to_string()))?;
        let recomputed = crate::collection_root(domain, collection_key, top)
            .map_err(|error| PersistenceError::Staging(error.to_string()))?;
        if recomputed != collection.new_collection_root {
            return Err(PersistenceError::NewCollectionRootMismatch);
        }
        Ok(())
    }
    fn apply_retirement(
        &self,
        tx: &(impl DbTxMut + DbTx),
        collection_key: CollectionKey,
        retirement: &RetirementBatch,
        catalog_leaf: Option<B256>,
    ) -> Result<(), PersistenceError> {
        let domain = crate::CeDomain::try_from(retirement.domain_id)
            .map_err(|_| PersistenceError::InvalidTopologyIdentity)?;
        if domain != crate::CeDomain::Tribute
            || catalog_leaf != Some(retirement.parent_collection_root)
        {
            return Err(PersistenceError::ParentCatalogRootMismatch);
        }
        let persisted =
            read_collection_roots(tx, &self.path, collection_key, domain.shard_count())?;
        if persisted.as_ref() != Some(&retirement.parent_shard_roots) {
            return Err(PersistenceError::ParentShardRootsMismatch);
        }
        let top = aggregate_b256_shard_roots(&retirement.parent_shard_roots)
            .map_err(|error| PersistenceError::Staging(error.to_string()))?;
        if crate::collection_root(domain, collection_key, top)
            .map_err(|error| PersistenceError::Staging(error.to_string()))?
            != retirement.parent_collection_root
        {
            return Err(PersistenceError::ParentCatalogRootMismatch);
        }
        delete_collection_records(tx, self, collection_key)?;
        Ok(())
    }
    fn apply_collection_shards(
        &self,
        tx: &(impl DbTxMut + DbTx),
        collection_key: CollectionKey,
        shard_set: &ProvisionalShardSetBatch,
    ) -> Result<(), PersistenceError> {
        for shard_index in 0..shard_set.shard_count {
            let namespace = TreeNamespace::CollectionShard(collection_key, shard_index);
            let position =
                usize::try_from(shard_index).map_err(|_| PersistenceError::InvalidShardCount {
                    actual: shard_index,
                })?;
            if let Some(shard) = shard_set.changed_shards.get(&shard_index) {
                apply_tree_changes(tx, self, namespace, shard)?;
            }
            tx.put::<tables::CeTreeRoots>(
                namespace.encode(),
                shard_set.new_shard_roots[position].as_slice().to_vec(),
            )
            .map_err(|error| self.db_error(error))?;
        }
        Ok(())
    }
}
