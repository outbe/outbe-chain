use alloy_primitives::B256;
use alloy_primitives::{Address, U256};
use outbe_compressed_entities::{
    body_commitment, encode_nod_bucket_v1, encode_nod_item_v1, encode_tribute_v1,
    AuthenticatedParentTree, CeBodyAudit, CeDomain, EntityRef, FinalLeafMutation,
    MdbxAuthenticatedTree, NodBucketBodyV1, NodItemBodyV1, StoredBody, TributeBodyV1, WwdEntityId,
};
use outbe_compressed_entities::{
    sealed_root, CeAuditError, CeAuditLimits, CeAuditVisitor, CeAuditWork, CeMdbx, CeMdbxReadOnly,
    CeTopologyV1, EnvironmentIdentity, ExactParentIdentity, FinalizedMarker, LeafValue, TreeKey,
    TreeNamespace, ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_primitives::time::WorldwideDay;

#[test]
fn external_consumer_can_name_every_tree_visitor_type_and_audit_genesis() {
    struct NoLeaves;
    impl CeAuditVisitor for NoLeaves {
        fn visit_leaf(
            &mut self,
            _: TreeNamespace,
            _: TreeKey,
            _: LeafValue,
        ) -> Result<(), CeAuditError> {
            panic!("native genesis contains no leaves");
        }
    }
    let source = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let hash = B256::repeat_byte(3);
    let identity = EnvironmentIdentity {
        local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
        chain_id: 10,
        genesis_hash: hash,
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        topology: CeTopologyV1.encode(),
        tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".into(),
        vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".into(),
    };
    let marker = FinalizedMarker {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: hash,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: sealed_root(B256::ZERO).unwrap(),
    };
    drop(CeMdbx::open(source.path(), identity.clone(), marker).unwrap());
    let reader = CeMdbxReadOnly::open(source.path(), identity).unwrap();
    let path = scratch.path().join("audit");
    let work = CeAuditWork::create(&path, CeAuditLimits::default()).unwrap();
    let report = reader
        .audit_exact(
            ExactParentIdentity {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                block_number: 0,
                block_hash: hash,
                root: marker.new_root,
            },
            &work,
            &mut NoLeaves,
        )
        .unwrap();
    assert_eq!(report.sealed_root, marker.new_root);
    assert_eq!((report.trees, report.leaves), (1, 0));
    drop(work);
    assert!(!path.exists());
    assert!(source
        .path()
        .join("compressed_entities/smt/mdbx.dat")
        .exists());
}

type Body = (CeDomain, WwdEntityId, Vec<u8>);

fn bodies() -> Vec<Body> {
    let day = WorldwideDay::new(20_260_717);
    let tribute = TributeBodyV1 {
        tribute_id: WwdEntityId::from_day_and_digest(day, [0x31; 32]),
        owner: Address::repeat_byte(0x41),
        worldwide_day: day,
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
        exclude_from_intex_issuance: false,
    };
    let item = NodItemBodyV1 {
        is_settled: false,
        nod_id: WwdEntityId::from_day_and_digest(day, [0x32; 32]),
        owner: Address::repeat_byte(0x42),
        gratis_load_minor: U256::from(1),
        worldwide_day: day,
        league_id: 7,
        floor_price_minor: U256::from(2),
        bucket_key: B256::repeat_byte(0x43),
        issuance_currency: 840,
        reference_currency: 978,
        issued_at: 123,
    };
    let bucket = NodBucketBodyV1 {
        settled_nods: 0,
        bucket_key: B256::repeat_byte(0x33),
        worldwide_day: day,
        floor_price_minor: U256::from(10),
        is_qualified: true,
        entry_price_minor: U256::from(11),
        reference_currency: 840,
    };
    [
        (
            CeDomain::Tribute,
            tribute.tribute_id,
            encode_tribute_v1(&tribute).unwrap(),
        ),
        (
            CeDomain::NodItem,
            item.nod_id,
            encode_nod_item_v1(&item).unwrap(),
        ),
        (
            CeDomain::NodBucket,
            bucket.entity_id(),
            encode_nod_bucket_v1(&bucket).unwrap(),
        ),
    ]
    .into_iter()
    .map(|(domain, id, payload)| (domain, id, StoredBody::new_v1(payload).unwrap().encode()))
    .collect()
}

fn audit_bodies(committed: &[Body], supplied: &[Body]) -> Result<u64, CeAuditError> {
    audit_bodies_after_deleting(committed, supplied, &[])
}

fn audit_bodies_after_deleting(
    committed: &[Body],
    supplied: &[Body],
    deleted: &[EntityRef],
) -> Result<u64, CeAuditError> {
    let source = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let hash = B256::repeat_byte(3);
    let identity = EnvironmentIdentity {
        local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
        chain_id: 10,
        genesis_hash: hash,
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        topology: CeTopologyV1.encode(),
        tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".into(),
        vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".into(),
    };
    let genesis = FinalizedMarker {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: hash,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: sealed_root(B256::ZERO).unwrap(),
    };
    let db = std::sync::Arc::new(CeMdbx::open(source.path(), identity.clone(), genesis).unwrap());
    let mutations: Vec<_> = committed
        .iter()
        .map(|(domain, id, bytes)| {
            let stored = StoredBody::decode(bytes).unwrap();
            FinalLeafMutation {
                entity: match domain {
                    CeDomain::Tribute => EntityRef::Tribute(*id),
                    CeDomain::NodItem => EntityRef::NodItem(*id),
                    CeDomain::NodBucket => EntityRef::NodBucket(*id),
                },
                final_leaf: Some(
                    body_commitment(
                        ACTIVE_COMMITMENT_SCHEME,
                        stored.schema_version(),
                        *id,
                        stored.payload(),
                    )
                    .unwrap(),
                ),
            }
        })
        .collect();
    let parent = MdbxAuthenticatedTree::open(
        db.clone(),
        ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 0,
            block_hash: hash,
            root: genesis.new_root,
        },
    )
    .unwrap();
    let staged = parent
        .prepare_seal(1, &mutations, &[])
        .unwrap()
        .freeze(B256::repeat_byte(4));
    db.apply_finalized(&staged).unwrap();
    let mut marker = staged.marker(ACTIVE_COMMITMENT_SCHEME);
    drop(parent);
    if !deleted.is_empty() {
        let parent = MdbxAuthenticatedTree::open(
            db.clone(),
            ExactParentIdentity {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                block_number: marker.height,
                block_hash: marker.block_hash,
                root: marker.new_root,
            },
        )
        .unwrap();
        let mutations: Vec<_> = deleted
            .iter()
            .map(|entity| FinalLeafMutation {
                entity: *entity,
                final_leaf: None,
            })
            .collect();
        let staged = parent
            .prepare_seal(2, &mutations, &[])
            .unwrap()
            .freeze(B256::repeat_byte(5));
        db.apply_finalized(&staged).unwrap();
        marker = staged.marker(ACTIVE_COMMITMENT_SCHEME);
    }
    drop(db);
    let reader = CeMdbxReadOnly::open(source.path(), identity).unwrap();
    let work = CeAuditWork::create(
        scratch.path().join("audit"),
        CeAuditLimits {
            records_per_run: 2,
            merge_fan_in: 2,
        },
    )
    .unwrap();
    let mut audit = CeBodyAudit::create(&work)?;
    reader.audit_exact(
        ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: marker.height,
            block_hash: marker.block_hash,
            root: marker.new_root,
        },
        &work,
        &mut audit,
    )?;
    for (domain, id, bytes) in supplied {
        audit.push_body(*domain, *id, bytes)?;
    }
    let report = audit.finish()?;
    assert!(report.peak_buffered_records <= 2);
    Ok(report.bodies)
}

#[test]
fn complete_body_population_covers_tribute_items_and_buckets_across_runs() {
    let committed = bodies();
    let supplied: Vec<_> = committed.iter().rev().cloned().collect();
    assert_eq!(audit_bodies(&committed, &supplied).unwrap(), 3);
    assert_eq!(audit_bodies(&[], &[]).unwrap(), 0);
}

#[test]
fn body_population_accepts_multiple_days_and_materialized_empty_collections() {
    let mut committed = bodies();
    let mut tribute = outbe_compressed_entities::decode_stored_tribute_v1(&committed[0].2).unwrap();
    for (day, digest) in [(20_260_717, 0x61), (20_260_718, 0x62), (20_260_719, 0x63)] {
        tribute.worldwide_day = WorldwideDay::new(day);
        tribute.tribute_id = WwdEntityId::from_day_and_digest(tribute.worldwide_day, [digest; 32]);
        committed.push((
            CeDomain::Tribute,
            tribute.tribute_id,
            StoredBody::new_v1(encode_tribute_v1(&tribute).unwrap())
                .unwrap()
                .encode(),
        ));
    }
    let supplied: Vec<_> = committed.iter().rev().cloned().collect();
    assert_eq!(audit_bodies(&committed, &supplied).unwrap(), 6);
    let deleted = [
        EntityRef::NodItem(committed[1].1),
        EntityRef::NodBucket(committed[2].1),
        EntityRef::Tribute(committed[4].1),
    ];
    let supplied: Vec<_> = committed
        .iter()
        .enumerate()
        .filter(|(i, _)| ![1, 2, 4].contains(i))
        .map(|(_, row)| row.clone())
        .collect();
    assert_eq!(
        audit_bodies_after_deleting(&committed, &supplied, &deleted).unwrap(),
        3
    );
}

#[test]
fn body_population_rejects_missing_extra_changed_and_duplicate_records() {
    let committed = bodies();
    assert!(
        audit_bodies(&committed, &committed[..2]).is_err(),
        "missing bucket"
    );
    assert!(
        audit_bodies(&committed[..2], &committed).is_err(),
        "uncommitted bucket"
    );
    let mut duplicate = committed.clone();
    duplicate.push(committed[0].clone());
    assert!(
        audit_bodies(&committed, &duplicate).is_err(),
        "duplicate across runs"
    );
    let mut changed = committed.clone();
    let mut bucket = outbe_compressed_entities::decode_stored_nod_bucket_v1(&changed[2].2).unwrap();
    bucket.entry_price_minor += U256::from(1);
    changed[2].2 = StoredBody::new_v1(encode_nod_bucket_v1(&bucket).unwrap())
        .unwrap()
        .encode();
    assert!(
        audit_bodies(&committed, &changed).is_err(),
        "changed canonical payload"
    );
}

#[test]
fn body_population_rejects_wrong_identity_and_unsupported_envelope() {
    let committed = bodies();
    let mut changed = committed.clone();
    changed[0].1 = WwdEntityId::from_day_and_digest(WorldwideDay::new(20_260_717), [0x77; 32]);
    assert!(audit_bodies(&committed, &changed).is_err());
    let mut changed = committed.clone();
    let stored = StoredBody::decode(&changed[0].2).unwrap();
    changed[0].2 = StoredBody::new(2, stored.payload().to_vec())
        .unwrap()
        .encode();
    assert!(audit_bodies(&committed, &changed).is_err());
    changed[0].2 = vec![0xff];
    assert!(audit_bodies(&committed, &changed).is_err());
}

#[test]
fn record_comparison_is_bidirectional_and_propagates_source_errors() {
    let scratch = tempfile::tempdir().unwrap();
    let work = CeAuditWork::create(
        scratch.path().join("audit"),
        CeAuditLimits {
            records_per_run: 2,
            merge_fan_in: 2,
        },
    )
    .unwrap();
    let rows = || [[3, 9], [2, 8], [1, 7]].map(Ok);
    work.compare_records(1, rows(), rows().into_iter().rev())
        .unwrap();
    assert!(work
        .compare_records(1, rows(), [[1, 7], [2, 8]].map(Ok))
        .is_err());
    assert!(work
        .compare_records(1, [[1, 7], [2, 8]].map(Ok), rows())
        .is_err());
    assert!(work
        .compare_records(1, rows(), [[1, 7], [2, 8], [3, 8]].map(Ok))
        .is_err());
    assert!(work
        .compare_records(1, rows(), [[1, 7], [1, 7], [3, 9]].map(Ok))
        .is_err());
    let error = work
        .compare_records(
            1,
            rows(),
            [Err(CeAuditError::Invalid("source interrupted".into()))],
        )
        .unwrap_err();
    assert!(error.to_string().contains("source interrupted"));
}

#[test]
fn audit_work_rejects_unrepresentable_tree_buffer_before_creating_scratch() {
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("audit");
    assert!(CeAuditWork::create(
        &path,
        CeAuditLimits {
            records_per_run: usize::MAX / 64 + 1,
            merge_fan_in: 2,
        }
    )
    .is_err());
    assert!(!path.exists());
}
