use super::*;

#[test]
fn restart_rows_distinguish_equal_behind_ahead_and_conflict() {
    let current = marker(7);
    let equal = DurableFinalizedCheckpoint {
        commitment_scheme_version: 1,
        height: 7,
        block_hash: current.block_hash,
        root: current.new_root,
        parent_block_hash: current.parent_block_hash,
        parent_root: current.parent_root,
        consensus_finalized_height: 7,
    };
    assert_eq!(
        classify_restart(current, equal),
        RestartClassification::Equal
    );

    let behind = DurableFinalizedCheckpoint {
        height: 9,
        consensus_finalized_height: 9,
        ..equal
    };
    assert_eq!(
        classify_restart(current, behind),
        RestartClassification::Behind {
            first_missing: 8,
            target: 9
        }
    );

    let ahead_marker = marker(10);
    assert_eq!(
        classify_restart(ahead_marker, behind),
        RestartClassification::Ahead
    );
    assert_eq!(
        classify_restart(
            current,
            DurableFinalizedCheckpoint {
                block_hash: b256(99),
                ..equal
            }
        ),
        RestartClassification::Conflict
    );
    assert_eq!(
        classify_restart(
            current,
            DurableFinalizedCheckpoint {
                parent_root: b256(98),
                ..equal
            }
        ),
        RestartClassification::Conflict
    );
}

#[test]
fn ack_and_retention_require_known_successful_commit() {
    for stage in [
        FinalizationStage::Delivered,
        FinalizationStage::MarshalDurable,
        FinalizationStage::RethFinalized,
        FinalizationStage::RethPersisted,
        FinalizationStage::ProviderVerified,
        FinalizationStage::CeCommitUnknown,
        FinalizationStage::CeCommitted,
        FinalizationStage::RetentionAdvanced,
    ] {
        assert!(!stage.marshal_ack_allowed());
    }
    assert!(FinalizationStage::CacheRemoved.marshal_ack_allowed());
    assert!(FinalizationStage::CeCommitUnknown.restart_requires_marker_classification());

    let previous = marker(7);
    let committed = marker(8);
    let cursor = CeRetentionCursor::from_verified_marker(previous);
    cursor
        .advance_after_known_commit(previous, committed)
        .unwrap();
    assert_eq!(cursor.height(), 8);
    assert!(cursor
        .advance_after_known_commit(previous, committed)
        .is_err());
}

#[test]
fn no_change_batch_still_carries_a_complete_next_marker() {
    let batch = crate::staging::ProvisionalTreeBatch::new_fixture_single_collection(
        8,
        b256(7),
        b256(18),
        b256(18),
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap()
    .freeze(b256(8));
    let next = batch.marker(1);
    assert_eq!(next.height, 8);
    assert_eq!(next.parent_root, next.new_root);
    assert_eq!(next.block_hash, b256(8));
}

#[test]
fn reopen_rejects_environment_identity_drift() {
    let directory = tempfile::tempdir().unwrap();
    let genesis = FinalizedMarker {
        commitment_scheme_version: 1,
        height: 0,
        block_hash: identity().genesis_hash,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: sealed_root(B256::ZERO).unwrap(),
    };
    let store = CeMdbx::open(directory.path(), identity(), genesis).unwrap();
    drop(store);
    let mut wrong = identity();
    wrong.chain_id += 1;
    assert!(matches!(
        CeMdbx::open(directory.path(), wrong, genesis),
        Err(PersistenceError::EnvironmentIdentityMismatch { .. })
    ));
}

#[test]
fn v3_initializes_only_catalog_and_rejects_topology_drift() {
    let directory = tempfile::tempdir().unwrap();
    let identity = sharded_identity(K_PROVISIONAL);
    let genesis = sharded_genesis(K_PROVISIONAL);
    assert_ne!(genesis.new_root, B256::ZERO);

    let store = CeMdbx::open(directory.path(), identity.clone(), genesis).unwrap();
    let snapshot = store.open_snapshot().unwrap();
    assert_eq!(
        snapshot.tree_root(TreeNamespace::Catalog).unwrap(),
        Some(B256::ZERO)
    );
    assert_eq!(snapshot.marker().unwrap(), genesis);
    drop(snapshot);
    drop(store);

    let mut wrong = identity;
    wrong.topology.push(0);
    assert!(matches!(
        CeMdbx::open(directory.path(), wrong, genesis),
        Err(PersistenceError::InvalidTopologyIdentity)
    ));
}
