use super::*;
use crate::{
    schema::Collection,
    sharding::shard_index,
    smt::{derive_tree_key, PoseidonSmt, TreeLeaf},
    CeDomain, CollectionBatch, ProvisionalCatalogBatch, ProvisionalShardBatch,
    ProvisionalShardSetBatch, TreeChange,
};
use std::collections::BTreeMap;

type KeyedLeaves = Vec<(crate::smt::TreeKey, crate::Commitment)>;
type GroupedLeaves = BTreeMap<crate::CollectionKey, (CeDomain, KeyedLeaves)>;

pub(super) fn prepare(
    block_number: u64,
    parent_root: B256,
    mutations: &[FinalLeafMutation],
) -> Result<crate::ProvisionalTreeBatch> {
    let grouped = group_mutations(mutations)?;
    let mut changed_collections = BTreeMap::new();
    let mut catalog_updates = Vec::new();
    let mut catalog_leaf_changes = BTreeMap::new();
    for (collection_key, (domain, keyed)) in grouped {
        let (new_collection_root, collection_batch) =
            prepare_collection(collection_key, domain, keyed)?;
        changed_collections.insert(
            collection_key,
            crate::CollectionOperation::Mutate(collection_batch),
        );
        let catalog_key = crate::smt::TreeKey::from_be_bytes(*collection_key.as_bytes())
            .map_err(|error| fatal_scope(error.to_string()))?;
        let catalog_leaf = TreeLeaf::from_be_bytes(new_collection_root.0)
            .map_err(|error| fatal_scope(error.to_string()))?;
        catalog_updates.push((catalog_key, catalog_leaf));
        let persisted_catalog_key =
            crate::persistence::TreeKey::try_from(B256::from(*collection_key.as_bytes()))
                .map_err(|error| fatal_scope(error.to_string()))?;
        catalog_leaf_changes.insert(
            persisted_catalog_key,
            TreeChange::Set(
                crate::persistence::LeafValue::try_from(new_collection_root)
                    .map_err(|error| fatal_scope(error.to_string()))?,
            ),
        );
    }
    let mut catalog = PoseidonSmt::empty();
    let new_catalog_root = B256::from(
        catalog
            .update_all(catalog_updates)
            .map_err(|error| fatal_scope(error.to_string()))?
            .as_bytes(),
    );
    let catalog_batch = (!changed_collections.is_empty()).then_some(ProvisionalCatalogBatch {
        parent_catalog_root: B256::ZERO,
        new_catalog_root,
        branch_changes: BTreeMap::new(),
        leaf_changes: catalog_leaf_changes,
    });
    crate::ProvisionalTreeBatch::new(
        block_number,
        B256::ZERO,
        parent_root,
        crate::sealed_root(new_catalog_root).map_err(|error| fatal_scope(error.to_string()))?,
        B256::ZERO,
        new_catalog_root,
        changed_collections,
        catalog_batch,
    )
    .map_err(|error| fatal_scope(error.to_string()))
}

fn group_mutations(mutations: &[FinalLeafMutation]) -> Result<GroupedLeaves> {
    let mut grouped = BTreeMap::new();
    for mutation in mutations {
        let (domain, collection, entity_id) = match mutation.entity {
            EntityRef::Tribute(id) => (CeDomain::Tribute, Collection::Tribute, id),
            EntityRef::NodItem(id) => (CeDomain::NodItem, Collection::NodItem, id),
            EntityRef::NodBucket(id) => (CeDomain::NodBucket, Collection::NodBucket, id),
        };
        let Some(commitment) = mutation.final_leaf else {
            continue;
        };
        let collection_key = crate::collection_key(domain, entity_id)
            .map_err(|error| fatal_scope(error.to_string()))?;
        let key = derive_tree_key(collection, entity_id)
            .map_err(|error| fatal_scope(error.to_string()))?;
        grouped
            .entry(collection_key)
            .or_insert_with(|| (domain, Vec::new()))
            .1
            .push((key, commitment));
    }

    Ok(grouped)
}

fn prepare_collection(
    collection_key: crate::CollectionKey,
    domain: CeDomain,
    keyed: KeyedLeaves,
) -> Result<(B256, CollectionBatch)> {
    let mut by_shard: BTreeMap<u32, Vec<(crate::smt::TreeKey, crate::Commitment)>> =
        BTreeMap::new();
    for (key, commitment) in keyed {
        let shard = shard_index(key, domain.shard_count())
            .map_err(|error| fatal_scope(error.to_string()))?;
        by_shard.entry(shard).or_default().push((key, commitment));
    }
    let parent_roots = vec![B256::ZERO; domain.shard_count() as usize];
    let mut new_roots = parent_roots.clone();
    let mut changed_shards = BTreeMap::new();
    for (shard, updates) in by_shard {
        let (new_root, batch) = prepare_shard(updates)?;
        new_roots[shard as usize] = new_root;
        changed_shards.insert(shard, batch);
    }
    let parent_top = crate::empty_shard_top_root(domain.shard_count())
        .map_err(|error| fatal_scope(error.to_string()))?;
    let new_top = crate::sharding::aggregate_b256_shard_roots(&new_roots)
        .map_err(|error| fatal_scope(error.to_string()))?;
    let new_collection_root = crate::collection_root(domain, collection_key, new_top)
        .map_err(|error| fatal_scope(error.to_string()))?;
    let shard_set = ProvisionalShardSetBatch::new(
        domain.shard_count(),
        parent_top,
        new_top,
        parent_roots,
        new_roots,
        changed_shards,
    )
    .map_err(|error| fatal_scope(error.to_string()))?;
    let batch = CollectionBatch::new(domain, collection_key, None, new_collection_root, shard_set)
        .map_err(|error| fatal_scope(error.to_string()))?;
    Ok((new_collection_root, batch))
}

fn prepare_shard(updates: KeyedLeaves) -> Result<(B256, ProvisionalShardBatch)> {
    let mut tree = PoseidonSmt::empty();
    let smt_updates = updates
        .iter()
        .map(|(key, commitment)| {
            TreeLeaf::from_be_bytes(*commitment.as_bytes()).map(|leaf| (*key, leaf))
        })
        .collect::<core::result::Result<Vec<_>, _>>()
        .map_err(|error| fatal_scope(error.to_string()))?;
    let new_root = B256::from(
        tree.update_all(smt_updates)
            .map_err(|error| fatal_scope(error.to_string()))?
            .as_bytes(),
    );
    let mut leaf_changes = BTreeMap::new();
    for (key, commitment) in updates {
        leaf_changes.insert(
            crate::persistence::TreeKey::try_from(B256::from(key.as_bytes()))
                .map_err(|error| fatal_scope(error.to_string()))?,
            TreeChange::Set(
                crate::persistence::LeafValue::try_from(B256::from(*commitment.as_bytes()))
                    .map_err(|error| fatal_scope(error.to_string()))?,
            ),
        );
    }
    let batch = ProvisionalShardBatch::new(B256::ZERO, new_root, BTreeMap::new(), leaf_changes)
        .map_err(|error| fatal_scope(error.to_string()))?;
    Ok((new_root, batch))
}
