use super::*;

fn legacy_collection_namespace(shard: u32) -> TreeNamespace {
    TreeNamespace::CollectionShard(CollectionKey::try_from(B256::ZERO).unwrap(), shard)
}

fn key_in_shard(shard: u32, ordinal: usize) -> TreeKey {
    (0..=u8::MAX)
        .map(|value| TreeKey::try_from(b256(value)).unwrap())
        .filter(|candidate| {
            let smt_key = crate::smt::TreeKey::from_be_bytes(candidate.encode()).unwrap();
            crate::sharding::shard_index(smt_key, K_PROVISIONAL).unwrap() == shard
        })
        .nth(ordinal)
        .unwrap()
}

#[test]
fn mdbx_applies_contiguous_batches_atomically_and_reopens_exact_marker() {
    let directory = tempfile::tempdir().unwrap();
    let genesis = FinalizedMarker {
        commitment_scheme_version: crate::ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: identity().genesis_hash,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: sealed_root(B256::ZERO).unwrap(),
    };
    let store = CeMdbx::open(directory.path(), identity(), genesis).unwrap();
    let tree_key = key_in_shard(0, 0);
    let leaf = LeafValue::try_from(b256(4)).unwrap();
    let branch_key = BranchKey::new(1, b256(5)).unwrap();
    let branch = BranchNode {
        left: MergeValue::Value(FieldValue::try_from(b256(6)).unwrap()),
        right: MergeValue::Value(FieldValue::try_from(b256(7)).unwrap()),
    };
    let batch = crate::staging::ProvisionalTreeBatch::new_fixture_single_collection(
        1,
        genesis.block_hash,
        b256(8),
        BTreeMap::from([(branch_key, TreeChange::Set(branch))]),
        BTreeMap::from([(tree_key, TreeChange::Set(leaf))]),
    )
    .unwrap()
    .freeze(b256(41));

    let applied = store.apply_finalized(&batch).unwrap();
    assert_eq!(
        applied,
        ApplyOutcome::Applied(batch.marker(crate::ACTIVE_COMMITMENT_SCHEME))
    );
    assert_eq!(
        store.apply_finalized(&batch).unwrap(),
        ApplyOutcome::AlreadyApplied(batch.marker(crate::ACTIVE_COMMITMENT_SCHEME))
    );
    let snapshot = store.open_snapshot().unwrap();
    assert_eq!(
        snapshot.marker().unwrap(),
        batch.marker(crate::ACTIVE_COMMITMENT_SCHEME)
    );
    assert_eq!(
        snapshot
            .read_leaf(legacy_collection_namespace(0), tree_key)
            .unwrap(),
        Some(leaf)
    );
    assert_eq!(
        snapshot
            .read_branch(legacy_collection_namespace(0), branch_key)
            .unwrap(),
        Some(branch)
    );

    drop(snapshot);
    drop(store);
    let reopened = CeMdbx::open(directory.path(), identity(), genesis).unwrap();
    assert_eq!(
        reopened.marker().unwrap(),
        batch.marker(crate::ACTIVE_COMMITMENT_SCHEME)
    );
}

#[test]
fn mdbx_rejects_gap_conflict_and_bad_batch_before_persistent_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let genesis = FinalizedMarker {
        commitment_scheme_version: crate::ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: identity().genesis_hash,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: sealed_root(B256::ZERO).unwrap(),
    };
    let store = CeMdbx::open(directory.path(), identity(), genesis).unwrap();
    let gap = crate::staging::ProvisionalTreeBatch::new_fixture_single_collection(
        2,
        genesis.block_hash,
        b256(51),
        BTreeMap::new(),
        BTreeMap::from([(
            key_in_shard(0, 0),
            TreeChange::Set(LeafValue::try_from(b256(2)).unwrap()),
        )]),
    )
    .unwrap()
    .freeze(b256(52));
    assert!(matches!(
        store.apply_finalized(&gap),
        Err(PersistenceError::NonContiguousFinalizedApply { .. })
    ));

    let mut malformed = crate::staging::ProvisionalTreeBatch::new_fixture_single_collection(
        1,
        genesis.block_hash,
        b256(51),
        BTreeMap::new(),
        BTreeMap::from([(
            key_in_shard(0, 0),
            TreeChange::Set(LeafValue::try_from(b256(2)).unwrap()),
        )]),
    )
    .unwrap()
    .freeze(b256(51));
    malformed.encoded_size = 1;
    assert!(matches!(
        store.apply_finalized(&malformed),
        Err(PersistenceError::StagingSource(error)) if matches!(*error, crate::staging::StagingError::EncodedSizeMismatch { .. })
    ));
    assert_eq!(store.marker().unwrap(), genesis);
}

#[test]
fn open_snapshot_remains_on_one_mdbx_read_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let genesis = FinalizedMarker {
        commitment_scheme_version: crate::ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: identity().genesis_hash,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: sealed_root(B256::ZERO).unwrap(),
    };
    let store = CeMdbx::open(directory.path(), identity(), genesis).unwrap();
    let old_snapshot = store.open_snapshot().unwrap();
    let tree_key = key_in_shard(0, 1);
    let leaf = LeafValue::try_from(b256(11)).unwrap();
    let batch = crate::staging::ProvisionalTreeBatch::new_fixture_single_collection(
        1,
        genesis.block_hash,
        b256(12),
        BTreeMap::new(),
        BTreeMap::from([(tree_key, TreeChange::Set(leaf))]),
    )
    .unwrap()
    .freeze(b256(61));
    store.apply_finalized(&batch).unwrap();

    assert_eq!(old_snapshot.marker().unwrap(), genesis);
    assert_eq!(
        old_snapshot
            .read_leaf(legacy_collection_namespace(0), tree_key)
            .unwrap(),
        None
    );
    let new_snapshot = store.open_snapshot().unwrap();
    assert_eq!(
        new_snapshot.marker().unwrap(),
        batch.marker(crate::ACTIVE_COMMITMENT_SCHEME)
    );
    assert_eq!(
        new_snapshot
            .read_leaf(legacy_collection_namespace(0), tree_key)
            .unwrap(),
        Some(leaf)
    );
}

#[test]
fn v3_collection_leaf_namespaces_are_isolated_by_typed_prefix() {
    let directory = tempfile::tempdir().unwrap();
    let identity = sharded_identity(K_PROVISIONAL);
    let genesis = sharded_genesis(K_PROVISIONAL);
    let store = CeMdbx::open(directory.path(), identity, genesis).unwrap();
    let key = TreeKey::try_from(b256(3)).unwrap();
    let first = LeafValue::try_from(b256(4)).unwrap();
    let second = LeafValue::try_from(b256(5)).unwrap();
    let tx = store.db.tx_mut().unwrap();
    tx.put::<tables::CeLeaves>(
        prefixed_key(legacy_collection_namespace(1), &key.encode()),
        first.encode().to_vec(),
    )
    .unwrap();
    tx.put::<tables::CeLeaves>(
        prefixed_key(legacy_collection_namespace(2), &key.encode()),
        second.encode().to_vec(),
    )
    .unwrap();
    tx.commit().unwrap();

    let snapshot = store.open_snapshot().unwrap();
    assert_eq!(
        snapshot.read_leaf(TreeNamespace::Catalog, key).unwrap(),
        None
    );
    assert_eq!(
        snapshot
            .read_leaf(legacy_collection_namespace(1), key)
            .unwrap(),
        Some(first)
    );
    assert_eq!(
        snapshot
            .read_leaf(legacy_collection_namespace(2), key)
            .unwrap(),
        Some(second)
    );
}

#[test]
fn v3_applies_two_changed_shards_and_catalog_leaf_under_one_marker() {
    let directory = tempfile::tempdir().unwrap();
    let identity = sharded_identity(K_PROVISIONAL);
    let genesis = sharded_genesis(K_PROVISIONAL);
    let store = CeMdbx::open(directory.path(), identity, genesis).unwrap();
    let key_one = TreeKey::try_from(b256(1)).unwrap();
    let key_two = TreeKey::try_from(b256(2)).unwrap();
    let leaf_one = LeafValue::try_from(b256(31)).unwrap();
    let leaf_two = LeafValue::try_from(b256(32)).unwrap();
    let mut new_roots = vec![B256::ZERO; K_PROVISIONAL as usize];
    new_roots[1] = b256(21);
    new_roots[2] = b256(22);
    let parent_roots = vec![B256::ZERO; K_PROVISIONAL as usize];
    let parent_top = aggregate_b256_shard_roots(&parent_roots).unwrap();
    let new_top = aggregate_b256_shard_roots(&new_roots).unwrap();
    let collection_key = CollectionKey::try_from(B256::ZERO).unwrap();
    let shard_set = ProvisionalShardSetBatch::new(
        K_PROVISIONAL,
        crate::staging::RootTransition {
            parent: parent_top,
            new: new_top,
        },
        parent_roots,
        new_roots,
        BTreeMap::from([
            (
                1,
                ProvisionalShardBatch::new(
                    B256::ZERO,
                    b256(21),
                    BTreeMap::new(),
                    BTreeMap::from([(key_one, TreeChange::Set(leaf_one))]),
                )
                .unwrap(),
            ),
            (
                2,
                ProvisionalShardBatch::new(
                    B256::ZERO,
                    b256(22),
                    BTreeMap::new(),
                    BTreeMap::from([(key_two, TreeChange::Set(leaf_two))]),
                )
                .unwrap(),
            ),
        ]),
    )
    .unwrap();
    let new_collection_root = collection_root(CeDomain::Tribute, collection_key, new_top).unwrap();
    let collection = CollectionBatch::new(
        CeDomain::Tribute,
        collection_key,
        None,
        new_collection_root,
        shard_set,
    )
    .unwrap();
    let parent_catalog_root = B256::ZERO;
    let new_catalog_root = b256(50);
    let catalog_key = TreeKey::try_from(B256::from(*collection_key.as_bytes())).unwrap();
    let batch = ProvisionalTreeBatch::new(
        1,
        genesis.block_hash,
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
        Some(ProvisionalCatalogBatch {
            parent_catalog_root,
            new_catalog_root,
            branch_changes: BTreeMap::new(),
            leaf_changes: BTreeMap::from([(
                catalog_key,
                TreeChange::Set(LeafValue::try_from(new_collection_root).unwrap()),
            )]),
        }),
    )
    .unwrap()
    .freeze(b256(50));

    assert_eq!(batch.changed_shard_count(), 2);
    assert_eq!(batch.leaf_change_count(), 3);
    assert_eq!(
        store.apply_finalized(&batch).unwrap(),
        ApplyOutcome::Applied(batch.marker(crate::ACTIVE_COMMITMENT_SCHEME))
    );
    let snapshot = store.open_snapshot().unwrap();
    assert_eq!(
        snapshot.marker().unwrap(),
        batch.marker(crate::ACTIVE_COMMITMENT_SCHEME)
    );
    assert_eq!(
        snapshot
            .read_leaf(TreeNamespace::CollectionShard(collection_key, 1), key_one)
            .unwrap(),
        Some(leaf_one)
    );
    assert_eq!(
        snapshot
            .read_leaf(TreeNamespace::CollectionShard(collection_key, 2), key_two)
            .unwrap(),
        Some(leaf_two)
    );
    assert_eq!(
        snapshot
            .read_leaf(TreeNamespace::CollectionShard(collection_key, 2), key_one)
            .unwrap(),
        None
    );
}

#[test]
fn exporter_environment_is_really_read_only_at_the_mdbx_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let expected_identity = identity();
    let genesis = FinalizedMarker {
        commitment_scheme_version: expected_identity.commitment_scheme_version,
        height: 0,
        block_hash: expected_identity.genesis_hash,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: crate::sealed_root(B256::ZERO).unwrap(),
    };
    let writer = CeMdbx::open(directory.path(), expected_identity.clone(), genesis).unwrap();
    let expected = writer.marker().unwrap();
    drop(writer);
    let reader = CeMdbxReadOnly::open(directory.path(), expected_identity).unwrap();

    assert_eq!(reader.marker().unwrap(), expected);
    assert!(reader.db.tx_mut().is_err());
    assert!(reader
        .open_exact(ExactParentIdentity {
            commitment_scheme_version: expected.commitment_scheme_version,
            block_number: expected.height,
            block_hash: B256::repeat_byte(0xFF),
            root: expected.new_root,
        })
        .is_err());
}

#[test]
fn caller_supplied_legacy_environment_is_rejected_before_initialization() {
    let directory = tempfile::tempdir().unwrap();
    let mut old = identity();
    old.commitment_scheme_version = 1;
    let genesis = FinalizedMarker {
        commitment_scheme_version: 1,
        ..sharded_genesis(K_PROVISIONAL)
    };
    assert!(matches!(
        CeMdbx::open(directory.path(), old.clone(), genesis),
        Err(PersistenceError::UnsupportedCommitmentScheme { actual: 1 })
    ));
    assert!(matches!(
        CeMdbxReadOnly::open(directory.path(), old),
        Err(PersistenceError::UnsupportedCommitmentScheme { actual: 1 })
    ));
    let mut old_format = identity();
    old_format.tree_format = "ckb-smt-v0.6.1-poseidon-catalog-v3".into();
    assert!(matches!(
        CeMdbx::open(directory.path(), old_format, sharded_genesis(K_PROVISIONAL)),
        Err(PersistenceError::UnsupportedTreeFormat { .. })
    ));
    let store = CeMdbx::open(directory.path(), identity(), sharded_genesis(K_PROVISIONAL)).unwrap();
    assert_eq!(store.marker().unwrap(), sharded_genesis(K_PROVISIONAL));
}

#[test]
fn stored_legacy_identity_is_rejected_without_rewriting_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let genesis = sharded_genesis(K_PROVISIONAL);
    let store = CeMdbx::open(directory.path(), identity(), genesis).unwrap();
    let mut old = identity();
    old.commitment_scheme_version = 1;
    old.tree_format = "ckb-smt-v0.6.1-poseidon-catalog-v3".into();
    let old_bytes = old.encode().unwrap();
    let old_marker = FinalizedMarker {
        commitment_scheme_version: 1,
        ..genesis
    }
    .encode()
    .to_vec();
    let tx = store.db.tx_mut().unwrap();
    tx.put::<tables::CeMetadata>(IDENTITY_KEY.to_vec(), old_bytes.clone())
        .unwrap();
    tx.put::<tables::CeMetadata>(LAST_APPLIED_KEY.to_vec(), old_marker.clone())
        .unwrap();
    tx.commit().unwrap();
    drop(store);
    assert!(matches!(
        CeMdbx::open(directory.path(), identity(), genesis),
        Err(PersistenceError::EnvironmentIdentityMismatch { .. })
    ));
    assert!(CeMdbxReadOnly::open(directory.path(), identity()).is_err());
    let db = reth_db::open_db_read_only(
        directory.path().join(CE_SMT_RELATIVE_PATH),
        reth_db::mdbx::DatabaseArguments::new(reth_db::ClientVersion::default()),
    )
    .unwrap();
    let tx = db.tx().unwrap();
    assert_eq!(
        tx.get::<tables::CeMetadata>(IDENTITY_KEY.to_vec()).unwrap(),
        Some(old_bytes)
    );
    assert_eq!(
        tx.get::<tables::CeMetadata>(LAST_APPLIED_KEY.to_vec())
            .unwrap(),
        Some(old_marker)
    );
}
