//! Prepare shard and catalog batches over one exact immutable parent.

use super::*;

impl MdbxAuthenticatedTree {
    pub(super) fn prepare_batch(
        &self,
        block_number: u64,
        mutations: &[FinalLeafMutation],
        retirements: &[crate::PartitionRef],
    ) -> Result<ProvisionalTreeBatch> {
        if block_number != self.identity.block_number.saturating_add(1) {
            return Err(CompressedEntitiesTreeCorruptionError::SealBlockNotParentPlusOne.into());
        }
        // Seal preparation is a side-effect-free projection and may be
        // repeated after a provisional terminal decision. Build it from a
        // fresh mutable session over the same authenticated snapshot instead
        // of consuming the read/session cache owned by this scope.
        let preparation = Self::from_view(self.view.clone())?;
        let mut state = preparation
            .state
            .lock()
            .map_err(|_| CompressedEntitiesTreeCorruptionError::CatalogSessionLockPoisoned)?;
        let grouped = group_mutations(mutations)?;

        let result = (|| {
            let mut changed_collections = BTreeMap::new();
            let mut catalog_updates = Vec::new();
            for (collection_key, (domain, keyed)) in grouped {
                let Some(collection_batch) =
                    self.prepare_collection(&mut state, (domain, collection_key), keyed)?
                else {
                    continue;
                };
                let new_collection_root = collection_batch.new_collection_root;
                let catalog_key = TreeKey::from_be_bytes(*collection_key.as_bytes())
                    .map_err(CompressedEntitiesTreeCorruptionError::from)?;
                let catalog_leaf = TreeLeaf::from_be_bytes(new_collection_root.0)
                    .map_err(CompressedEntitiesTreeCorruptionError::from)?;
                catalog_updates.push((catalog_key, catalog_leaf));
                changed_collections.insert(
                    collection_key,
                    crate::CollectionOperation::Mutate(collection_batch),
                );
            }

            self.prepare_retirements(
                &mut state,
                retirements,
                &mut changed_collections,
                &mut catalog_updates,
            )?;

            self.freeze_catalog(
                &mut state,
                block_number,
                changed_collections,
                catalog_updates,
            )
        })();
        if result.is_err() {
            state.verified_leaves.clear();
        }
        result
    }
    fn prepare_collection(
        &self,
        state: &mut SessionState,
        collection: (CeDomain, CollectionKey),
        mut keyed: Vec<CollectionMutation>,
    ) -> Result<Option<CollectionBatch>> {
        let (domain, collection_key) = collection;
        self.ensure_collection(state, domain, collection_key)?;
        let session = state
            .collections
            .get_mut(&collection_key)
            .ok_or(CompressedEntitiesTreeCorruptionError::MissingCollectionSession)?;
        self.ensure_mutable_trees(collection_key, session);
        keyed.sort_by_key(|(_, key, _)| *key);
        let mut by_shard: BTreeMap<ShardIndex, Vec<(TreeKey, TreeLeaf)>> = BTreeMap::new();
        for (_, key, leaf) in keyed {
            let shard = shard_index(key, domain.shard_count())
                .map_err(CompressedEntitiesTreeCorruptionError::from)?;
            by_shard.entry(shard).or_default().push((key, leaf));
        }
        let trees = session
            .trees
            .take()
            .ok_or(CompressedEntitiesTreeCorruptionError::CollectionShardSetAlreadySealed)?;
        let mut new_roots = session.parent_shard_roots.clone();
        let mut changed_shards = BTreeMap::new();
        for (position, tree) in trees.into_iter().enumerate() {
            let shard = position as u32;
            let Some(updates) = by_shard.remove(&shard) else {
                continue;
            };
            let (new_root, batch) =
                prepare_shard(tree, session.parent_shard_roots[position], updates)?;
            new_roots[position] = new_root;
            if let Some(batch) = batch {
                changed_shards.insert(shard, batch);
            }
        }
        if !by_shard.is_empty() {
            return Err(
                CompressedEntitiesTreeCorruptionError::DerivedMutationShardNotPrepared.into(),
            );
        }
        if changed_shards.is_empty() {
            return Ok(None);
        }
        let parent_top = aggregate_b256_shard_roots(&session.parent_shard_roots)
            .map_err(CompressedEntitiesTreeCorruptionError::from)?;
        let new_top = aggregate_b256_shard_roots(&new_roots)
            .map_err(CompressedEntitiesTreeCorruptionError::from)?;
        let new_collection_root = collection_root(domain, collection_key, new_top)
            .map_err(CompressedEntitiesTreeCorruptionError::from)?;
        let shard_set = ProvisionalShardSetBatch::new(
            domain.shard_count(),
            crate::staging::RootTransition {
                parent: parent_top,
                new: new_top,
            },
            session.parent_shard_roots.clone(),
            new_roots,
            changed_shards,
        )
        .map_err(CompressedEntitiesTreeCorruptionError::from)?;
        let collection_batch = CollectionBatch::new(
            domain,
            collection_key,
            session.parent_collection_root,
            new_collection_root,
            shard_set,
        )
        .map_err(CompressedEntitiesTreeCorruptionError::from)?;
        Ok(Some(collection_batch))
    }
    fn prepare_retirements(
        &self,
        state: &mut SessionState,
        retirements: &[crate::PartitionRef],
        changed_collections: &mut BTreeMap<CollectionKey, crate::CollectionOperation>,
        catalog_updates: &mut Vec<(TreeKey, TreeLeaf)>,
    ) -> Result<()> {
        let mut unique_retirements = BTreeSet::new();
        for partition in retirements {
            let (domain, collection_key) = crate::partition_collection_key(*partition)
                .map_err(CompressedEntitiesTreeCorruptionError::from)?;
            if !unique_retirements.insert(collection_key) {
                return Err(
                    CompressedEntitiesTreeCorruptionError::DuplicateCollectionRetirementAtSeal
                        .into(),
                );
            }
            if changed_collections.contains_key(&collection_key) {
                return Err(
                    CompressedEntitiesTreeCorruptionError::CollectionMutationAndRetirementCoexist
                        .into(),
                );
            }
            self.ensure_collection(state, domain, collection_key)?;
            let session = state
                .collections
                .get(&collection_key)
                .ok_or(CompressedEntitiesTreeCorruptionError::MissingRetirementCollectionSession)?;
            let parent_collection_root = session.parent_collection_root.ok_or(
                CompressedEntitiesTreeCorruptionError::RetirementCollectionAbsentFromParentCatalog,
            )?;
            let retirement = crate::RetirementBatch::new(
                domain,
                collection_key,
                parent_collection_root,
                session.parent_shard_roots.clone(),
            )
            .map_err(CompressedEntitiesTreeCorruptionError::from)?;
            let catalog_key = TreeKey::from_be_bytes(*collection_key.as_bytes())
                .map_err(CompressedEntitiesTreeCorruptionError::from)?;
            catalog_updates.push((catalog_key, TreeLeaf::ZERO));
            changed_collections.insert(
                collection_key,
                crate::CollectionOperation::Retire(retirement),
            );
        }
        Ok(())
    }
    fn freeze_catalog(
        &self,
        state: &mut SessionState,
        block_number: u64,
        changed_collections: BTreeMap<CollectionKey, crate::CollectionOperation>,
        catalog_updates: Vec<(TreeKey, TreeLeaf)>,
    ) -> Result<ProvisionalTreeBatch> {
        let mut catalog_tree = state
            .catalog_tree
            .take()
            .ok_or(CompressedEntitiesTreeCorruptionError::CatalogSealedMoreThanOnce)?;
        let new_catalog_root = if catalog_updates.is_empty() {
            self.view.catalog_root()
        } else {
            let keys = catalog_updates
                .iter()
                .map(|(key, _)| *key)
                .collect::<Vec<_>>();
            let parents = keys
                .iter()
                .map(|key| {
                    catalog_tree
                        .get(*key)
                        .map(|leaf| (*key, leaf))
                        .map_err(|err| CompressedEntitiesTreeCorruptionError::from(err).into())
                })
                .collect::<Result<Vec<_>>>()?;
            let proof = catalog_tree
                .prove(keys)
                .map_err(CompressedEntitiesTreeCorruptionError::from)?;
            catalog_tree
                .verify(
                    TreeRoot::from_be_bytes(self.view.catalog_root().0)
                        .map_err(CompressedEntitiesTreeCorruptionError::from)?,
                    &proof,
                    parents,
                )
                .map_err(CompressedEntitiesTreeCorruptionError::from)?;
            B256::from(
                catalog_tree
                    .update_all(catalog_updates)
                    .map_err(CompressedEntitiesTreeCorruptionError::from)?
                    .as_bytes(),
            )
        };
        let catalog_batch = catalog_tree
            .into_store()
            .freeze_shard(new_catalog_root)
            .map_err(CompressedEntitiesTreeCorruptionError::from)?
            .map(|batch| ProvisionalCatalogBatch {
                parent_catalog_root: batch.parent_shard_root,
                new_catalog_root: batch.new_shard_root,
                branch_changes: batch.branch_changes,
                leaf_changes: batch.leaf_changes,
            });
        let parent_r_sealed = sealed_root(self.view.catalog_root())
            .map_err(CompressedEntitiesTreeCorruptionError::from)?;
        let new_r_sealed =
            sealed_root(new_catalog_root).map_err(CompressedEntitiesTreeCorruptionError::from)?;
        ProvisionalTreeBatch::new(
            block_number,
            self.identity.block_hash,
            crate::staging::CatalogRoots {
                sealed: crate::staging::RootTransition {
                    parent: parent_r_sealed,
                    new: new_r_sealed,
                },
                catalog: crate::staging::RootTransition {
                    parent: self.view.catalog_root(),
                    new: new_catalog_root,
                },
            },
            changed_collections,
            catalog_batch,
        )
        .map_err(|err| CompressedEntitiesTreeCorruptionError::from(err).into())
    }
}
fn group_mutations(mutations: &[FinalLeafMutation]) -> Result<GroupedCollectionMutations> {
    let mut grouped = GroupedCollectionMutations::new();
    let mut unique_entities = BTreeSet::new();
    let mut unique_keys = BTreeMap::new();
    for mutation in mutations {
        if !unique_entities.insert(mutation.entity) {
            return Err(
                CompressedEntitiesTreeCorruptionError::DuplicateEntityIdentityAtSeal.into(),
            );
        }
        let (domain, _, id) = entity_parts(mutation.entity);
        let collection =
            collection_key(domain, id).map_err(CompressedEntitiesTreeCorruptionError::from)?;
        let key = entity_tree_key(mutation.entity)?;
        if let Some(other) = unique_keys.insert((collection, key), mutation.entity) {
            if other != mutation.entity {
                return Err(CompressedEntitiesTreeCorruptionError::TreeKeyCollision.into());
            }
        }
        let leaf = mutation
            .final_leaf
            .map_or(Ok(TreeLeaf::ZERO), |commitment| {
                TreeLeaf::from_be_bytes(*commitment.as_bytes()).map_err(|err| {
                    PrecompileError::from(CompressedEntitiesTreeCorruptionError::from(err))
                })
            })?;
        grouped
            .entry(collection)
            .or_insert_with(|| (domain, Vec::new()))
            .1
            .push((mutation.entity, key, leaf));
    }
    Ok(grouped)
}

fn prepare_shard(
    mut tree: PoseidonSmt<StagingCkbStore>,
    parent_root: B256,
    updates: Vec<(TreeKey, TreeLeaf)>,
) -> Result<(B256, Option<crate::staging::ProvisionalShardBatch>)> {
    let keys = updates.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let parents = keys
        .iter()
        .map(|key| {
            tree.get(*key)
                .map(|leaf| (*key, leaf))
                .map_err(|err| CompressedEntitiesTreeCorruptionError::from(err).into())
        })
        .collect::<Result<Vec<_>>>()?;
    let proof = tree
        .prove(keys)
        .map_err(CompressedEntitiesTreeCorruptionError::from)?;
    tree.verify(
        TreeRoot::from_be_bytes(parent_root.0)
            .map_err(CompressedEntitiesTreeCorruptionError::from)?,
        &proof,
        parents.clone(),
    )
    .map_err(CompressedEntitiesTreeCorruptionError::from)?;
    let effective = updates
        .into_iter()
        .zip(parents)
        .filter_map(|((key, final_leaf), (parent_key, parent_leaf))| {
            (key == parent_key && final_leaf != parent_leaf).then_some((key, final_leaf))
        })
        .collect::<Vec<_>>();
    if !effective.is_empty() {
        tree.update_all(effective)
            .map_err(CompressedEntitiesTreeCorruptionError::from)?;
    }
    let new_root = B256::from(
        tree.root()
            .map_err(CompressedEntitiesTreeCorruptionError::from)?
            .as_bytes(),
    );
    let batch = tree
        .into_store()
        .freeze_shard(new_root)
        .map_err(CompressedEntitiesTreeCorruptionError::from)?;
    Ok((new_root, batch))
}
