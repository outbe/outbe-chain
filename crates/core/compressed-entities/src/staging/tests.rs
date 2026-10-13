use super::*;
use crate::persistence::{FieldValue, MergeValue};

#[derive(Debug)]
struct MemorySnapshot {
    marker: FinalizedMarker,
    branches: BTreeMap<BranchKey, BranchNode>,
    leaves: BTreeMap<TreeKey, LeafValue>,
}

impl FinalizedTreeSnapshot for MemorySnapshot {
    fn marker(&self) -> Result<FinalizedMarker, PersistenceError> {
        Ok(self.marker)
    }

    fn tree_root(&self, namespace: TreeNamespace) -> Result<Option<B256>, PersistenceError> {
        Ok((namespace == TreeNamespace::Catalog).then_some(b256(17)))
    }

    fn collection_has_records(&self, _collection: CollectionKey) -> Result<bool, PersistenceError> {
        Ok(false)
    }

    fn collection_root_count(&self, _collection: CollectionKey) -> Result<usize, PersistenceError> {
        Ok(0)
    }

    fn collection_leaf_count(&self, _collection: CollectionKey) -> Result<usize, PersistenceError> {
        Ok(0)
    }

    fn read_branch(
        &self,
        namespace: TreeNamespace,
        key: BranchKey,
    ) -> Result<Option<BranchNode>, PersistenceError> {
        assert_eq!(namespace, TreeNamespace::Catalog);
        Ok(self.branches.get(&key).copied())
    }

    fn read_leaf(
        &self,
        namespace: TreeNamespace,
        key: TreeKey,
    ) -> Result<Option<LeafValue>, PersistenceError> {
        assert_eq!(namespace, TreeNamespace::Catalog);
        Ok(self.leaves.get(&key).copied())
    }
}

fn b256(last: u8) -> B256 {
    let mut bytes = [0_u8; 32];
    bytes[31] = last;
    B256::from(bytes)
}

fn marker() -> FinalizedMarker {
    FinalizedMarker {
        commitment_scheme_version: 1,
        height: 7,
        block_hash: b256(7),
        parent_block_hash: b256(6),
        parent_root: b256(16),
        new_root: sealed_root(b256(17)).unwrap(),
    }
}

fn exact() -> ExactParentIdentity {
    ExactParentIdentity {
        commitment_scheme_version: 1,
        block_number: 7,
        block_hash: b256(7),
        root: sealed_root(b256(17)).unwrap(),
    }
}

fn leaf(last: u8) -> LeafValue {
    LeafValue::try_from(b256(last)).unwrap()
}

fn key(last: u8) -> TreeKey {
    TreeKey::try_from(b256(last)).unwrap()
}

fn key_in_shard(shard: ShardIndex, ordinal: usize) -> TreeKey {
    (0..=u8::MAX)
        .map(key)
        .filter(|candidate| {
            let smt_key = SmtTreeKey::from_be_bytes(candidate.encode()).unwrap();
            shard_index(smt_key, CeDomain::Tribute.shard_count()).unwrap() == shard
        })
        .nth(ordinal)
        .unwrap()
}

fn node(last: u8) -> BranchNode {
    BranchNode {
        left: MergeValue::Value(FieldValue::try_from(b256(last)).unwrap()),
        right: MergeValue::Value(FieldValue::try_from(b256(last + 1)).unwrap()),
    }
}

fn view(snapshot: MemorySnapshot) -> AuthenticatedCatalogView {
    AuthenticatedCatalogView::open(Box::new(snapshot), exact()).unwrap()
}

#[test]
fn exact_view_rejects_every_marker_identity_mismatch() {
    type MarkerMutation = Box<dyn Fn(&mut FinalizedMarker)>;

    let expected = exact();
    let mut mutations: Vec<MarkerMutation> = vec![
        Box::new(|value| value.commitment_scheme_version += 1),
        Box::new(|value| value.height += 1),
        Box::new(|value| value.block_hash = b256(99)),
        Box::new(|value| value.new_root = b256(99)),
    ];
    for mutate in &mut mutations {
        let mut wrong = marker();
        mutate(&mut wrong);
        let snapshot = MemorySnapshot {
            marker: wrong,
            branches: BTreeMap::new(),
            leaves: BTreeMap::new(),
        };
        assert!(AuthenticatedCatalogView::open(Box::new(snapshot), expected).is_err());
    }
}

#[test]
fn candidate_writes_shadow_base_without_mutating_snapshot() {
    let branch_key = BranchKey::new(9, b256(5)).unwrap();
    let leaf_key = key(6);
    let base_node = node(10);
    let base_leaf = leaf(12);
    let snapshot = MemorySnapshot {
        marker: marker(),
        branches: BTreeMap::from([(branch_key, base_node)]),
        leaves: BTreeMap::from([(leaf_key, base_leaf)]),
    };
    let mut store = StagingCkbStore::new(view(snapshot), TreeNamespace::Catalog, b256(17));

    assert_eq!(store.read_branch(branch_key).unwrap(), Some(base_node));
    assert_eq!(store.read_leaf(leaf_key).unwrap(), Some(base_leaf));
    store.write_branch(branch_key, node(20));
    store.delete_leaf(leaf_key);
    assert_eq!(store.read_branch(branch_key).unwrap(), Some(node(20)));
    assert_eq!(store.read_leaf(leaf_key).unwrap(), None);

    let batch = store.freeze_shard(b256(18)).unwrap().unwrap();
    assert_eq!(batch.branch_changes.len(), 1);
    assert_eq!(batch.leaf_changes.len(), 1);
}

#[test]
fn ckb_store_traits_round_trip_typed_records_and_reject_invalid_leaf() {
    let snapshot = MemorySnapshot {
        marker: marker(),
        branches: BTreeMap::new(),
        leaves: BTreeMap::new(),
    };
    let mut store = StagingCkbStore::new(view(snapshot), TreeNamespace::Catalog, b256(17));
    let vendor_key = VendorBranchKey::new(12, H256::from(key(4).encode()));
    let vendor_node = VendorBranchNode {
        left: VendorMergeValue::Value(H256::from(leaf(5).encode())),
        right: VendorMergeValue::MergeWithZero {
            base_node: H256::from(leaf(6).encode()),
            zero_bits: H256::from(key(7).encode()),
            zero_count: 0,
        },
    };
    StoreWriteOps::insert_branch(&mut store, vendor_key.clone(), vendor_node.clone()).unwrap();
    assert_eq!(
        StoreReadOps::<H256>::get_branch(&store, &vendor_key).unwrap(),
        Some(vendor_node)
    );

    let leaf_key = H256::from(key(8).encode());
    let leaf_value = H256::from(leaf(9).encode());
    StoreWriteOps::insert_leaf(&mut store, leaf_key, leaf_value).unwrap();
    assert_eq!(
        StoreReadOps::<H256>::get_leaf(&store, &leaf_key).unwrap(),
        Some(leaf_value)
    );
    assert!(StoreWriteOps::insert_leaf(&mut store, leaf_key, H256::zero()).is_err());
    assert!(
        StoreWriteOps::insert_leaf(&mut store, H256::from([u8::MAX; 32]), leaf_value,).is_err()
    );
}

#[test]
fn catalog_envelope_binds_collection_shards_derivations_and_canonical_size() {
    let domain = CeDomain::Tribute;
    let collection_key = CollectionKey::try_from(B256::ZERO).unwrap();
    let parent_roots = vec![B256::ZERO; 16];
    let parent_root = aggregate_b256_roots(&parent_roots).unwrap();
    let mut new_roots = parent_roots.clone();
    new_roots[1] = b256(9);
    let new_root = aggregate_b256_roots(&new_roots).unwrap();
    let changed = ProvisionalShardBatch::new(
        B256::ZERO,
        b256(9),
        BTreeMap::new(),
        BTreeMap::from([(key_in_shard(1, 0), TreeChange::Set(leaf(2)))]),
    )
    .unwrap();
    let shard_set = ProvisionalShardSetBatch::new(
        16,
        crate::staging::RootTransition {
            parent: parent_root,
            new: new_root,
        },
        parent_roots.clone(),
        new_roots.clone(),
        BTreeMap::from([(1, changed.clone())]),
    )
    .unwrap();
    assert_eq!(shard_set.shard_count(), 16);
    let new_collection_root = collection_root(domain, collection_key, new_root).unwrap();
    let collection =
        CollectionBatch::new(domain, collection_key, None, new_collection_root, shard_set).unwrap();
    let parent_catalog_root = B256::ZERO;
    let new_catalog_root = b256(20);
    let catalog_key = TreeKey::try_from(B256::from(*collection_key.as_bytes())).unwrap();
    let catalog_batch = ProvisionalCatalogBatch {
        parent_catalog_root,
        new_catalog_root,
        branch_changes: BTreeMap::new(),
        leaf_changes: BTreeMap::from([(
            catalog_key,
            TreeChange::Set(LeafValue::try_from(new_collection_root).unwrap()),
        )]),
    };
    let provisional = ProvisionalTreeBatch::new(
        8,
        b256(7),
        crate::staging::CatalogRoots {
            sealed: crate::staging::RootTransition {
                parent: sealed_root(parent_catalog_root).unwrap(),
                new: sealed_root(new_catalog_root).unwrap(),
            },
            catalog: crate::staging::RootTransition {
                parent: parent_catalog_root,
                new: new_catalog_root,
            },
        },
        BTreeMap::from([(collection_key, CollectionOperation::Mutate(collection))]),
        Some(catalog_batch),
    )
    .unwrap();
    assert_eq!(provisional.changed_shard_count(), 1);

    let staged = provisional.freeze(b256(8));
    assert_eq!(
        staged.parent_root(),
        sealed_root(parent_catalog_root).unwrap()
    );
    assert_eq!(staged.new_root(), sealed_root(new_catalog_root).unwrap());
    staged.validate_encoded_size().unwrap();
    let canonical = staged.canonical_bytes().unwrap();
    assert_eq!(canonical.len(), staged.encoded_size());
    assert_eq!(
        &canonical[canonical.len() - 8..],
        &u64::try_from(staged.encoded_size()).unwrap().to_be_bytes()
    );
    assert_eq!(
        crate::bench_support::candidate_checksum(&staged),
        alloy_primitives::keccak256(canonical)
    );

    assert!(ProvisionalShardSetBatch::new(
        16,
        crate::staging::RootTransition {
            parent: parent_root,
            new: new_root
        },
        vec![B256::ZERO; 15],
        new_roots.clone(),
        BTreeMap::from([(1, changed.clone())])
    )
    .is_err());
    assert!(ProvisionalShardSetBatch::new(
        16,
        crate::staging::RootTransition {
            parent: parent_root,
            new: new_root
        },
        parent_roots.clone(),
        new_roots.clone(),
        BTreeMap::from([(2, changed.clone())])
    )
    .is_err());
    let misderived = ProvisionalShardBatch::new(
        B256::ZERO,
        b256(9),
        BTreeMap::new(),
        BTreeMap::from([(key(2), TreeChange::Set(leaf(2)))]),
    )
    .unwrap();
    assert!(ProvisionalShardSetBatch::new(
        16,
        crate::staging::RootTransition {
            parent: parent_root,
            new: new_root
        },
        parent_roots,
        new_roots,
        BTreeMap::from([(1, misderived)])
    )
    .is_err());
}

#[test]
fn adr011_retirement_operation_has_pinned_discriminant_size_and_checksum() {
    let domain = CeDomain::Tribute;
    let collection_key = CollectionKey::try_from(B256::ZERO).unwrap();
    let parent_shard_roots = vec![B256::ZERO; 16];
    let parent_top = aggregate_b256_roots(&parent_shard_roots).unwrap();
    let parent_collection_root = collection_root(domain, collection_key, parent_top).unwrap();
    let retirement = RetirementBatch::new(
        domain,
        collection_key,
        parent_collection_root,
        parent_shard_roots,
    )
    .unwrap();
    let parent_catalog_root = b256(30);
    let new_catalog_root = b256(31);
    let catalog_key = TreeKey::try_from(B256::from(*collection_key.as_bytes())).unwrap();
    let provisional = ProvisionalTreeBatch::new(
        8,
        b256(7),
        crate::staging::CatalogRoots {
            sealed: crate::staging::RootTransition {
                parent: sealed_root(parent_catalog_root).unwrap(),
                new: sealed_root(new_catalog_root).unwrap(),
            },
            catalog: crate::staging::RootTransition {
                parent: parent_catalog_root,
                new: new_catalog_root,
            },
        },
        BTreeMap::from([(collection_key, CollectionOperation::Retire(retirement))]),
        Some(ProvisionalCatalogBatch {
            parent_catalog_root,
            new_catalog_root,
            branch_changes: BTreeMap::new(),
            leaf_changes: BTreeMap::from([(catalog_key, TreeChange::Delete)]),
        }),
    )
    .unwrap();
    let staged = provisional.freeze(b256(8));
    let canonical = staged.canonical_bytes().unwrap();

    assert_eq!(canonical.len(), 901);
    assert_eq!(canonical[236], 1, "Retire discriminant must remain one");
    assert_eq!(
        alloy_primitives::keccak256(&canonical),
        alloy_primitives::b256!("a8f190e2fd539aa5dad1fe39a615792a0bcde0e658faf5d2849d2df8fb3d52e7")
    );
}

#[test]
fn publication_is_structurally_idempotent_and_never_evicts() {
    let provisional = ProvisionalTreeBatch::new_fixture_single_collection(
        8,
        b256(7),
        b256(18),
        BTreeMap::new(),
        BTreeMap::from([(key_in_shard(0, 0), TreeChange::Set(leaf(2)))]),
    )
    .unwrap();
    let batch = provisional.freeze(b256(8));
    let mut cache = CandidateCache::new(CandidateCacheLimits {
        max_candidates: 1,
        max_encoded_bytes: batch.encoded_size,
    });
    assert_eq!(
        cache.publish(batch.clone()).unwrap(),
        PublicationOutcome::Published
    );
    assert_eq!(
        cache.publish(batch.clone()).unwrap(),
        PublicationOutcome::AlreadyPublished
    );

    let competing = ProvisionalTreeBatch::new_fixture_single_collection(
        8,
        b256(7),
        b256(19),
        BTreeMap::new(),
        BTreeMap::from([(key_in_shard(0, 1), TreeChange::Set(leaf(3)))]),
    )
    .unwrap()
    .freeze(b256(9));
    assert!(matches!(
        cache.publish(competing),
        Err(StagingError::CacheCapacity)
    ));
    assert_eq!(cache.get(b256(8)).unwrap().as_ref(), &batch);
}

#[test]
fn same_hash_with_different_typed_batch_is_corruption() {
    let first = ProvisionalTreeBatch::new_fixture_single_collection(
        8,
        b256(7),
        b256(18),
        BTreeMap::new(),
        BTreeMap::from([(key_in_shard(0, 0), TreeChange::Set(leaf(2)))]),
    )
    .unwrap()
    .freeze(b256(8));
    let conflicting = ProvisionalTreeBatch::new_fixture_single_collection(
        8,
        b256(7),
        b256(19),
        BTreeMap::new(),
        BTreeMap::from([(key_in_shard(0, 0), TreeChange::Set(leaf(3)))]),
    )
    .unwrap()
    .freeze(b256(8));
    let mut cache = CandidateCache::new(CandidateCacheLimits {
        max_candidates: 2,
        max_encoded_bytes: 2_000,
    });
    cache.publish(first).unwrap();
    assert!(matches!(
        cache.publish(conflicting),
        Err(StagingError::ConflictingPublication { .. })
    ));
}

#[test]
fn finalization_and_restart_remove_only_by_explicit_policy() {
    let mut cache = CandidateCache::new(CandidateCacheLimits {
        max_candidates: 4,
        max_encoded_bytes: 20_000,
    });
    for (height, hash) in [(8, 8), (8, 9), (9, 10)] {
        cache
            .publish(
                ProvisionalTreeBatch::new_fixture_single_collection(
                    height,
                    b256(7),
                    b256(18),
                    BTreeMap::new(),
                    BTreeMap::from([(
                        key_in_shard(0, usize::from(hash - 8)),
                        TreeChange::Set(leaf(hash + 1)),
                    )]),
                )
                .unwrap()
                .freeze(b256(hash)),
            )
            .unwrap();
    }
    cache.remove_finalized(8);
    assert_eq!(cache.len(), 1);
    assert!(cache.get(b256(10)).is_some());
    cache.discard_all_on_restart();
    assert!(cache.is_empty());
    assert_eq!(cache.encoded_bytes(), 0);
}
