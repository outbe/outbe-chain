//! Authenticate staged shard, collection and catalog envelopes.

use super::*;

pub(super) fn validate_shard_envelope(
    shard_count: u32,
    roots: RootTransition,
    parent_shard_roots: &[B256],
    new_shard_roots: &[B256],
    changed_shards: &BTreeMap<ShardIndex, ProvisionalShardBatch>,
) -> Result<(), StagingError> {
    let RootTransition {
        parent: parent_root,
        new: new_root,
    } = roots;
    let expected_len = usize::try_from(shard_count).map_err(|_| {
        StagingError::from(crate::errors::ShardEnvelopeErrorReason::ShardCountNotRepresentable)
    })?;
    if shard_count == 0 || shard_count > 256 || !shard_count.is_power_of_two() {
        return Err(crate::errors::ShardEnvelopeErrorReason::InvalidShardCount.into());
    }
    if parent_shard_roots.len() != expected_len || new_shard_roots.len() != expected_len {
        return Err(crate::errors::ShardEnvelopeErrorReason::RootVectorLengthMismatch.into());
    }
    let parent = aggregate_b256_roots(parent_shard_roots)?;
    let new = aggregate_b256_roots(new_shard_roots)?;
    if parent != parent_root || new != new_root {
        return Err(crate::errors::ShardEnvelopeErrorReason::AggregateRootMismatch.into());
    }
    for index in 0..shard_count {
        let position = usize::try_from(index).map_err(|_| {
            StagingError::from(crate::errors::ShardEnvelopeErrorReason::ShardIndexOverflow)
        })?;
        validate_changed_shard(
            index,
            shard_count,
            parent_shard_roots[position],
            new_shard_roots[position],
            changed_shards.get(&index),
        )?;
    }
    if changed_shards.keys().any(|index| *index >= shard_count) {
        return Err(crate::errors::ShardEnvelopeErrorReason::ShardIndexOutOfRange.into());
    }
    Ok(())
}

pub(super) fn aggregate_b256_roots(roots: &[B256]) -> Result<B256, StagingError> {
    aggregate_b256_shard_roots(roots).map_err(|_| {
        StagingError::from(crate::errors::ShardEnvelopeErrorReason::InvalidShardRootAggregate)
    })
}

pub(super) fn validate_catalog_envelope(
    roots: CatalogRoots,
    changed_collections: &BTreeMap<CollectionKey, CollectionOperation>,
    catalog_batch: Option<&ProvisionalCatalogBatch>,
) -> Result<(), StagingError> {
    let RootTransition {
        parent: parent_catalog_root,
        new: new_catalog_root,
    } = roots.catalog;
    validate_catalog_roots(roots)?;
    match (changed_collections.is_empty(), catalog_batch) {
        (true, None) if parent_catalog_root == new_catalog_root => return Ok(()),
        (true, _) => {
            return Err(
                crate::errors::CatalogEnvelopeErrorReason::IdentityCandidateContainsCatalogChanges
                    .into(),
            );
        }
        (false, None) => {
            return Err(
                crate::errors::CatalogEnvelopeErrorReason::ChangedCollectionsRequireCatalogBatch
                    .into(),
            );
        }
        (false, Some(batch)) => {
            if batch.parent_catalog_root != parent_catalog_root
                || batch.new_catalog_root != new_catalog_root
                || parent_catalog_root == new_catalog_root
            {
                return Err(
                    crate::errors::CatalogEnvelopeErrorReason::CatalogBatchRootMismatch.into(),
                );
            }
            if batch.leaf_changes.len() != changed_collections.len() {
                return Err(
                    crate::errors::CatalogEnvelopeErrorReason::CatalogLeafChangesDoNotCoverChangedCollections
                        .into(),
                );
            }
            for (key, operation) in changed_collections {
                validate_collection_operation(*key, operation, batch)?;
            }
        }
    }
    Ok(())
}

fn validate_changed_shard(
    index: u32,
    shard_count: u32,
    parent: B256,
    new: B256,
    batch: Option<&ProvisionalShardBatch>,
) -> Result<(), StagingError> {
    match (parent != new, batch) {
        (true, Some(batch)) => {
            if batch.parent_shard_root != parent || batch.new_shard_root != new {
                return Err(
                    crate::errors::ShardEnvelopeErrorReason::ChangedShardRootMismatch.into(),
                );
            }
            for key in batch.leaf_changes.keys() {
                let smt_key = SmtTreeKey::from_be_bytes(key.encode()).map_err(|_| {
                    StagingError::from(crate::errors::ShardEnvelopeErrorReason::InvalidLeafKey)
                })?;
                if shard_index(smt_key, shard_count).map_err(|_| {
                    StagingError::from(
                        crate::errors::ShardEnvelopeErrorReason::InvalidShardDerivation,
                    )
                })? != index
                {
                    return Err(crate::errors::ShardEnvelopeErrorReason::MisderivedLeafShard.into());
                }
            }
        }
        (false, None) => {}
        (true, None) => {
            return Err(crate::errors::ShardEnvelopeErrorReason::MissingChangedShard.into());
        }
        (false, Some(_)) => {
            return Err(crate::errors::ShardEnvelopeErrorReason::UnchangedShardIncluded.into());
        }
    }
    Ok(())
}

fn validate_collection_operation(
    key: CollectionKey,
    operation: &CollectionOperation,
    batch: &ProvisionalCatalogBatch,
) -> Result<(), StagingError> {
    let tree_key = TreeKey::try_from(B256::from(key.as_bytes()))?;
    match operation {
        CollectionOperation::Mutate(collection) => {
            let domain = CeDomain::try_from(collection.domain_id).map_err(|_| {
                StagingError::from(
                    crate::errors::CatalogEnvelopeErrorReason::UnknownCollectionDomain,
                )
            })?;
            if collection.shard_set.shard_count != domain.shard_count() {
                return Err(
                    crate::errors::CatalogEnvelopeErrorReason::CollectionShardCountMismatch.into(),
                );
            }
            validate_shard_envelope(
                collection.shard_set.shard_count,
                RootTransition {
                    parent: collection.shard_set.parent_shard_top_root,
                    new: collection.shard_set.new_shard_top_root,
                },
                &collection.shard_set.parent_shard_roots,
                &collection.shard_set.new_shard_roots,
                &collection.shard_set.changed_shards,
            )?;
            let expected_parent = collection
                .parent_collection_root
                .map(|_| collection_root(domain, key, collection.shard_set.parent_shard_top_root))
                .transpose()
                .map_err(|_| {
                    StagingError::from(
                        crate::errors::CatalogEnvelopeErrorReason::InvalidParentCollection,
                    )
                })?;
            if expected_parent != collection.parent_collection_root
                || (collection.parent_collection_root.is_none()
                    && collection
                        .shard_set
                        .parent_shard_roots
                        .iter()
                        .any(|root| *root != B256::ZERO))
                || collection_root(domain, key, collection.shard_set.new_shard_top_root).map_err(
                    |_| {
                        StagingError::from(
                            crate::errors::CatalogEnvelopeErrorReason::InvalidNewCollection,
                        )
                    },
                )? != collection.new_collection_root
            {
                return Err(
                    crate::errors::CatalogEnvelopeErrorReason::CollectionRootMismatch.into(),
                );
            }
            let expected = LeafValue::try_from(collection.new_collection_root)?;
            if batch.leaf_changes.get(&tree_key) != Some(&TreeChange::Set(expected)) {
                return Err(
                    crate::errors::CatalogEnvelopeErrorReason::CatalogLeafNotChangedCollectionRoot
                        .into(),
                );
            }
        }
        CollectionOperation::Retire(retirement) => {
            let domain = CeDomain::try_from(retirement.domain_id).map_err(|_| {
                StagingError::from(
                    crate::errors::CatalogEnvelopeErrorReason::UnknownRetirementDomain,
                )
            })?;
            RetirementBatch::new(
                domain,
                key,
                retirement.parent_collection_root,
                retirement.parent_shard_roots.clone(),
            )?;
            if batch.leaf_changes.get(&tree_key) != Some(&TreeChange::Delete) {
                return Err(
                    crate::errors::CatalogEnvelopeErrorReason::RetiredCatalogLeafNotDeleted.into(),
                );
            }
        }
    }

    Ok(())
}

fn validate_catalog_roots(roots: CatalogRoots) -> Result<(), StagingError> {
    let parent_r_sealed = roots.sealed.parent;
    let new_r_sealed = roots.sealed.new;
    let parent_catalog_root = roots.catalog.parent;
    let new_catalog_root = roots.catalog.new;
    if sealed_root(parent_catalog_root).map_err(|_| {
        StagingError::from(
            crate::errors::CatalogEnvelopeErrorReason::InvalidParentSealedRootWrapper,
        )
    })? != parent_r_sealed
        || sealed_root(new_catalog_root).map_err(|_| {
            StagingError::from(
                crate::errors::CatalogEnvelopeErrorReason::InvalidNewSealedRootWrapper,
            )
        })? != new_r_sealed
    {
        return Err(crate::errors::CatalogEnvelopeErrorReason::SealedRootWrapperMismatch.into());
    }
    Ok(())
}
