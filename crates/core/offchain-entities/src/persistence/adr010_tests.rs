use reth_db::database::Database;
use reth_db::transaction::DbTx;
use reth_db::transaction::DbTxMut;

use std::sync::Arc;

use alloy_primitives::B256;

use super::*;
use crate::{
    api::{AuthenticatedParentTree, EntityRef, FinalLeafMutation},
    collection_key, sealed_root, CeDomain, CeTopologyV1, Commitment, MdbxAuthenticatedTree,
    WwdEntityId, ACTIVE_COMMITMENT_SCHEME, K_PROVISIONAL,
};

const VENDOR: &str = "ad555350c866b2265d87d2d7fbd146fbc918bfe5";

fn identity(genesis_hash: B256) -> EnvironmentIdentity {
    EnvironmentIdentity {
        local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
        chain_id: 10,
        genesis_hash,
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        topology: CeTopologyV1.encode(),
        tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
        vendor_revision: VENDOR.to_owned(),
    }
}

fn genesis(genesis_hash: B256) -> FinalizedMarker {
    FinalizedMarker {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: genesis_hash,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: sealed_root(B256::ZERO).unwrap(),
    }
}

fn tribute_id(day: u32, suffix: u8) -> WwdEntityId {
    let mut bytes = [0_u8; 32];
    bytes[..4].copy_from_slice(&day.to_be_bytes());
    bytes[31] = suffix;
    WwdEntityId::try_from(bytes.as_slice()).unwrap()
}

#[test]
fn v3_retirement_reclaims_all_prefixes_alongside_other_wwd_and_both_nod_mutations() {
    let directory = tempfile::tempdir().unwrap();
    let genesis_hash = B256::repeat_byte(0x10);
    let db = Arc::new(
        CeMdbx::open(
            directory.path(),
            identity(genesis_hash),
            genesis(genesis_hash),
        )
        .unwrap(),
    );
    let snapshot = db.open_snapshot().unwrap();
    assert_eq!(
        snapshot.tree_root(TreeNamespace::Catalog).unwrap(),
        Some(B256::ZERO)
    );

    let id = tribute_id(20_260_717, 1);
    let collection = collection_key(CeDomain::Tribute, id).unwrap();
    assert!(!snapshot.collection_has_records(collection).unwrap());
    for shard in 0..K_PROVISIONAL {
        assert_eq!(
            snapshot
                .tree_root(TreeNamespace::CollectionShard(collection, shard))
                .unwrap(),
            None
        );
    }
    drop(snapshot);

    let parent = MdbxAuthenticatedTree::open(
        Arc::clone(&db),
        ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 0,
            block_hash: genesis_hash,
            root: sealed_root(B256::ZERO).unwrap(),
        },
    )
    .unwrap();
    assert_eq!(
        parent
            .read_leaf_verified(
                EntityRef::Tribute(tribute_id(20_260_717, 9)),
                sealed_root(B256::ZERO).unwrap()
            )
            .unwrap(),
        None
    );
    assert!(!db
        .open_snapshot()
        .unwrap()
        .collection_has_records(collection)
        .unwrap());
    let commitment = Commitment::try_from(B256::with_last_byte(1).0).unwrap();
    let provisional = parent
        .prepare_seal(
            1,
            &[FinalLeafMutation {
                entity: EntityRef::Tribute(id),
                final_leaf: Some(commitment),
            }],
            &[],
        )
        .unwrap();
    let staged = provisional.freeze(B256::repeat_byte(0x11));
    assert_eq!(
        db.apply_finalized(&staged).unwrap(),
        ApplyOutcome::Applied(staged.marker(1))
    );

    let snapshot = db.open_snapshot().unwrap();
    assert_ne!(
        snapshot.tree_root(TreeNamespace::Catalog).unwrap(),
        Some(B256::ZERO)
    );
    assert!(snapshot.collection_has_records(collection).unwrap());
    for shard in 0..K_PROVISIONAL {
        assert!(snapshot
            .tree_root(TreeNamespace::CollectionShard(collection, shard))
            .unwrap()
            .is_some());
    }
    assert_eq!(
        snapshot
            .tree_root(TreeNamespace::CollectionShard(collection, K_PROVISIONAL))
            .unwrap(),
        None
    );
    let block_one_root = staged.new_root();
    let block_one_hash = staged.block_hash();
    drop(snapshot);

    let parent = MdbxAuthenticatedTree::open(
        Arc::clone(&db),
        ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 1,
            block_hash: block_one_hash,
            root: block_one_root,
        },
    )
    .unwrap();
    let emptied = parent
        .prepare_seal(
            2,
            &[FinalLeafMutation {
                entity: EntityRef::Tribute(id),
                final_leaf: None,
            }],
            &[],
        )
        .unwrap()
        .freeze(B256::repeat_byte(0x12));
    db.apply_finalized(&emptied).unwrap();
    let snapshot = db.open_snapshot().unwrap();
    assert!(snapshot.collection_has_records(collection).unwrap());
    for shard in 0..K_PROVISIONAL {
        assert_eq!(
            snapshot
                .tree_root(TreeNamespace::CollectionShard(collection, shard))
                .unwrap(),
            Some(B256::ZERO)
        );
    }
    let emptied_root = emptied.new_root();
    let emptied_hash = emptied.block_hash();
    drop(snapshot);

    let parent = MdbxAuthenticatedTree::open(
        Arc::clone(&db),
        ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 2,
            block_hash: emptied_hash,
            root: emptied_root,
        },
    )
    .unwrap();
    let nod_id = tribute_id(20_260_718, 2);
    let other_tribute_id = tribute_id(20_260_719, 3);
    let nod_collection = collection_key(CeDomain::NodItem, nod_id).unwrap();
    let bucket_collection = collection_key(CeDomain::NodBucket, nod_id).unwrap();
    let other_tribute_collection = collection_key(CeDomain::Tribute, other_tribute_id).unwrap();
    let staged_delete = parent
        .prepare_seal(
            3,
            &[
                FinalLeafMutation {
                    entity: EntityRef::NodItem(nod_id),
                    final_leaf: Some(Commitment::try_from(B256::with_last_byte(2).0).unwrap()),
                },
                FinalLeafMutation {
                    entity: EntityRef::NodBucket(nod_id),
                    final_leaf: Some(Commitment::try_from(B256::with_last_byte(3).0).unwrap()),
                },
                FinalLeafMutation {
                    entity: EntityRef::Tribute(other_tribute_id),
                    final_leaf: Some(Commitment::try_from(B256::with_last_byte(4).0).unwrap()),
                },
            ],
            &[crate::PartitionRef::TributeWwd(
                outbe_primitives::time::WorldwideDay::new(20_260_717),
            )],
        )
        .unwrap()
        .freeze(B256::repeat_byte(0x13));
    db.apply_finalized(&staged_delete).unwrap();
    let snapshot = db.open_snapshot().unwrap();
    let catalog_key = TreeKey::try_from(B256::from(*collection.as_bytes())).unwrap();
    assert!(snapshot
        .read_leaf(TreeNamespace::Catalog, catalog_key)
        .unwrap()
        .is_none());
    assert!(!snapshot.collection_has_records(collection).unwrap());
    assert!(snapshot.collection_has_records(nod_collection).unwrap());
    assert!(snapshot.collection_has_records(bucket_collection).unwrap());
    assert!(snapshot
        .collection_has_records(other_tribute_collection)
        .unwrap());
    for shard in 0..K_PROVISIONAL {
        assert_eq!(
            snapshot
                .tree_root(TreeNamespace::CollectionShard(collection, shard))
                .unwrap(),
            None
        );
    }
}

#[test]
fn topology_identity_is_canonical_and_mismatch_never_falls_back() {
    let directory = tempfile::tempdir().unwrap();
    let genesis_hash = B256::repeat_byte(0x20);
    let expected = identity(genesis_hash);
    drop(CeMdbx::open(directory.path(), expected.clone(), genesis(genesis_hash)).unwrap());

    let mut mismatched = expected;
    mismatched.topology.push(0);
    assert!(matches!(
        CeMdbx::open(directory.path(), mismatched, genesis(genesis_hash)),
        Err(PersistenceError::InvalidTopologyIdentity)
    ));
}

#[test]
fn one_block_atomically_creates_three_independent_domain_collections() {
    let directory = tempfile::tempdir().unwrap();
    let genesis_hash = B256::repeat_byte(0x30);
    let db = Arc::new(
        CeMdbx::open(
            directory.path(),
            identity(genesis_hash),
            genesis(genesis_hash),
        )
        .unwrap(),
    );
    let parent = MdbxAuthenticatedTree::open(
        Arc::clone(&db),
        ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 0,
            block_hash: genesis_hash,
            root: sealed_root(B256::ZERO).unwrap(),
        },
    )
    .unwrap();
    let id = tribute_id(20_260_718, 7);
    let mutations = [
        EntityRef::Tribute(id),
        EntityRef::NodItem(id),
        EntityRef::NodBucket(id),
    ]
    .map(|entity| FinalLeafMutation {
        entity,
        final_leaf: Some(
            Commitment::try_from(B256::with_last_byte(entity_kind(entity)).0).unwrap(),
        ),
    });
    let staged = parent
        .prepare_seal(1, &mutations, &[])
        .unwrap()
        .freeze(B256::repeat_byte(0x31));
    assert_eq!(staged.changed_collections.len(), 3);
    assert_eq!(staged.catalog_batch.as_ref().unwrap().leaf_changes.len(), 3);
    db.apply_finalized(&staged).unwrap();

    let snapshot = db.open_snapshot().unwrap();
    for domain in [CeDomain::Tribute, CeDomain::NodItem, CeDomain::NodBucket] {
        let key = collection_key(domain, id).unwrap();
        assert!(snapshot.collection_has_records(key).unwrap());
        assert!(snapshot
            .read_leaf(
                TreeNamespace::Catalog,
                TreeKey::try_from(B256::from(*key.as_bytes())).unwrap(),
            )
            .unwrap()
            .is_some());
    }
    assert_eq!(
        snapshot.marker().unwrap(),
        staged.marker(ACTIVE_COMMITMENT_SCHEME)
    );
}

fn entity_kind(entity: EntityRef) -> u8 {
    match entity {
        EntityRef::Tribute(_) => 1,
        EntityRef::NodItem(_) => 2,
        EntityRef::NodBucket(_) => 3,
    }
}

#[test]
fn v3_namespace_codec_is_typed_strict_and_order_preserving() {
    let key = collection_key(CeDomain::Tribute, tribute_id(20_260_719, 1)).unwrap();
    let catalog = TreeNamespace::Catalog.encode();
    let shard = TreeNamespace::CollectionShard(key, 15).encode();
    assert_eq!(catalog, vec![0]);
    assert_eq!(shard.len(), 37);
    assert_eq!(
        TreeNamespace::decode(&catalog).unwrap(),
        TreeNamespace::Catalog
    );
    assert_eq!(
        TreeNamespace::decode(&shard).unwrap(),
        TreeNamespace::CollectionShard(key, 15)
    );
    for malformed in [
        vec![2],
        vec![0, 0],
        TreeNamespace::CollectionShard(key, K_PROVISIONAL).encode(),
    ] {
        assert!(TreeNamespace::decode(&malformed).is_err());
    }
}

#[test]
fn present_collection_rejects_missing_and_extra_root_records() {
    for extra in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let genesis_hash = B256::repeat_byte(if extra { 0x41 } else { 0x40 });
        let db = Arc::new(
            CeMdbx::open(
                directory.path(),
                identity(genesis_hash),
                genesis(genesis_hash),
            )
            .unwrap(),
        );
        let id = tribute_id(20_260_720, 1);
        let collection = collection_key(CeDomain::Tribute, id).unwrap();
        let parent = MdbxAuthenticatedTree::open(
            Arc::clone(&db),
            ExactParentIdentity {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                block_number: 0,
                block_hash: genesis_hash,
                root: sealed_root(B256::ZERO).unwrap(),
            },
        )
        .unwrap();
        let staged = parent
            .prepare_seal(
                1,
                &[FinalLeafMutation {
                    entity: EntityRef::Tribute(id),
                    final_leaf: Some(Commitment::try_from(B256::with_last_byte(1).0).unwrap()),
                }],
                &[],
            )
            .unwrap()
            .freeze(B256::repeat_byte(0x42));
        db.apply_finalized(&staged).unwrap();

        let tx = db.db.tx_mut().unwrap();
        let namespace =
            TreeNamespace::CollectionShard(collection, if extra { K_PROVISIONAL } else { 0 });
        if extra {
            tx.put::<tables::CeTreeRoots>(namespace.encode(), B256::ZERO.as_slice().to_vec())
                .unwrap();
        } else {
            tx.delete::<tables::CeTreeRoots>(namespace.encode(), None)
                .unwrap();
        }
        tx.commit().unwrap();

        let reopened = MdbxAuthenticatedTree::open(
            Arc::clone(&db),
            ExactParentIdentity {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                block_number: 1,
                block_hash: staged.block_hash(),
                root: staged.new_root(),
            },
        )
        .unwrap();
        assert!(reopened
            .read_leaf_verified(EntityRef::Tribute(id), staged.new_root())
            .is_err());
    }
}

#[test]
fn catalog_non_membership_rejects_orphan_collection_prefix_records() {
    let directory = tempfile::tempdir().unwrap();
    let genesis_hash = B256::repeat_byte(0x50);
    let db = Arc::new(
        CeMdbx::open(
            directory.path(),
            identity(genesis_hash),
            genesis(genesis_hash),
        )
        .unwrap(),
    );
    let id = tribute_id(20_260_721, 1);
    let collection = collection_key(CeDomain::Tribute, id).unwrap();
    let key = TreeKey::try_from(B256::with_last_byte(1)).unwrap();
    let tx = db.db.tx_mut().unwrap();
    tx.put::<tables::CeLeaves>(
        prefixed_key(TreeNamespace::CollectionShard(collection, 0), &key.encode()),
        LeafValue::try_from(B256::with_last_byte(1))
            .unwrap()
            .encode()
            .to_vec(),
    )
    .unwrap();
    tx.commit().unwrap();

    let parent = MdbxAuthenticatedTree::open(
        Arc::clone(&db),
        ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 0,
            block_hash: genesis_hash,
            root: sealed_root(B256::ZERO).unwrap(),
        },
    )
    .unwrap();
    assert!(parent
        .read_leaf_verified(EntityRef::Tribute(id), sealed_root(B256::ZERO).unwrap())
        .is_err());
}

#[test]
fn identity_candidate_advances_only_marker_and_keeps_empty_catalog() {
    let directory = tempfile::tempdir().unwrap();
    let genesis_hash = B256::repeat_byte(0x60);
    let db = Arc::new(
        CeMdbx::open(
            directory.path(),
            identity(genesis_hash),
            genesis(genesis_hash),
        )
        .unwrap(),
    );
    let parent = MdbxAuthenticatedTree::open(
        Arc::clone(&db),
        ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 0,
            block_hash: genesis_hash,
            root: sealed_root(B256::ZERO).unwrap(),
        },
    )
    .unwrap();
    let staged = parent
        .prepare_seal(1, &[], &[])
        .unwrap()
        .freeze(B256::repeat_byte(0x61));
    assert!(staged.changed_collections.is_empty());
    assert!(staged.catalog_batch.is_none());
    assert_eq!(staged.parent_root(), staged.new_root());
    db.apply_finalized(&staged).unwrap();
    let snapshot = db.open_snapshot().unwrap();
    assert_eq!(
        snapshot.tree_root(TreeNamespace::Catalog).unwrap(),
        Some(B256::ZERO)
    );
    assert_eq!(
        snapshot.marker().unwrap(),
        staged.marker(ACTIVE_COMMITMENT_SCHEME)
    );
}
