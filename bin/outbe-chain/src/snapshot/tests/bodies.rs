use std::{fs, sync::Arc};

use alloy_consensus::{Header, Sealable};
use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    body_commitment, sealed_root, AuthenticatedParentTree, CeAuditError, CeAuditLimits,
    CeAuditReport, CeAuditWork, CeBodyAudit, CeMdbx, CeMdbxReadOnly, CeTopologyV1, EntityRef,
    EnvironmentIdentity, ExactParentIdentity, FinalLeafMutation, FinalizedMarker,
    MdbxAuthenticatedTree, StoredBody, WwdEntityId, ACTIVE_COMMITMENT_SCHEME,
    LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_nod::{NodBucketState, NodItemState, NodRepositoryWriter};
use outbe_offchain_data::{ProjectionCheckpoint, ProjectionState, STORAGE_SCHEMA_VERSION};
use outbe_offchain_storage::{Key, Namespace, RocksDbStorage, StorageWriter, Value};
use outbe_primitives::{
    reshare_artifact::{
        encode_outbe_block_artifacts, CompressedEntitiesRootArtifact, OutbeBlockArtifacts,
    },
    time::WorldwideDay,
    OutbeHeader,
};
use outbe_tribute::{
    RetainedTributeAuditEntry, RetainedTributeAuditVisitor, RetainedTributePin,
    RetainedTributeReader, TributeData, TributeRepositoryWriter,
};
use reth_ethereum::provider::db::mdbx::DatabaseArguments;

use super::super::{
    config::{parse_node_inputs, resolve_requested_layout, NativeReadSelection, RequestedLayout},
    validation::{
        bodies::{ProjectionBodyReport, ProjectionBodyView},
        ce::verify_ce,
        Incomplete,
    },
};
use super::headers::fingerprint;

fn tribute(seed: u8) -> TributeData {
    let day = WorldwideDay::new(20260901 + u32::from(seed));
    TributeData {
        tribute_id: WwdEntityId::from_day_and_digest(day, [seed; 32]),
        owner: Address::repeat_byte(seed),
        worldwide_day: day,
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
        exclude_from_intex_issuance: false,
    }
}

struct Fixture {
    source: tempfile::TempDir,
    layout: RequestedLayout,
    ce: CeMdbxReadOnly,
    header: OutbeHeader,
    bucket_id: WwdEntityId,
}

impl Fixture {
    fn new(populated: bool) -> Self {
        let source = tempfile::tempdir().unwrap();
        let args = super::layout::native_arguments(source.path());
        let config = source.path().join("configuration/offchain.toml");
        fs::write(
            &config,
            fs::read_to_string(&config)
                .unwrap()
                .replace("start_block = 17", "start_block = 0"),
        )
        .unwrap();
        let layout = resolve_requested_layout(
            &parse_node_inputs(args).unwrap(),
            NativeReadSelection { projection: true },
        )
        .unwrap();
        fs::create_dir_all(layout.chain_root.join("keys")).unwrap();
        fs::write(
            layout.chain_root.join("keys/sentinel.key"),
            b"protected identity",
        )
        .unwrap();
        let genesis_hash = layout.chain.genesis_hash();
        let identity = EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: layout.chain.chain().id(),
            genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".into(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".into(),
        };
        let genesis = FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: 0,
            block_hash: genesis_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: sealed_root(B256::ZERO).unwrap(),
        };
        drop(
            reth_ethereum::provider::db::create_db(
                layout.chain_root.join("compressed_entities/smt"),
                DatabaseArguments::test(),
            )
            .unwrap(),
        );
        let ce = Arc::new(CeMdbx::open(&layout.chain_root, identity.clone(), genesis).unwrap());
        let projection =
            Arc::new(RocksDbStorage::open(&layout.projection.as_ref().unwrap().root).unwrap());
        let mut header = layout.chain.genesis_header().clone();
        let mut bucket_id = WwdEntityId::ZERO;
        if populated {
            let tribute_writer =
                TributeRepositoryWriter::new(projection.clone(), projection.clone());
            let nod_writer = NodRepositoryWriter::new(projection.clone(), projection.clone());
            let mut bodies = Vec::new();
            for seed in [1, 2] {
                let body = tribute(seed);
                tribute_writer.put(&body).unwrap();
                let stored = StoredBody::new_v1(
                    outbe_compressed_entities::encode_tribute_v1(&outbe_tribute::canonical_body(
                        &body,
                    ))
                    .unwrap(),
                )
                .unwrap();
                bodies.push((EntityRef::Tribute(body.tribute_id), body.tribute_id, stored));
            }
            let day = WorldwideDay::new(20260905);
            let item = NodItemState {
                is_settled: false,
                nod_id: WwdEntityId::from_day_and_digest(day, [3; 32]),
                owner: Address::repeat_byte(3),
                gratis_load_minor: U256::from(1),
                worldwide_day: day,
                league_id: 7,
                floor_price_minor: U256::from(2),
                bucket_key: B256::repeat_byte(4),
                issuance_currency: 840,
                reference_currency: 978,
                issued_at: 123,
            };
            nod_writer.put_nod(&item).unwrap();
            bodies.push((
                EntityRef::NodItem(item.nod_id),
                item.nod_id,
                StoredBody::new_v1(
                    outbe_compressed_entities::encode_nod_item_v1(&outbe_nod::canonical_item(
                        &item,
                    ))
                    .unwrap(),
                )
                .unwrap(),
            ));
            let bucket = NodBucketState {
                settled_nods: 0,
                bucket_key: item.bucket_key,
                worldwide_day: day,
                floor_price_minor: U256::from(2),
                is_qualified: true,
                entry_price_minor: U256::from(3),
                reference_currency: 978,
            };
            nod_writer.put_bucket(&bucket).unwrap();
            let canonical = outbe_nod::canonical_bucket(&bucket);
            bucket_id = canonical.entity_id();
            bodies.push((
                EntityRef::NodBucket(bucket_id),
                bucket_id,
                StoredBody::new_v1(
                    outbe_compressed_entities::encode_nod_bucket_v1(&canonical).unwrap(),
                )
                .unwrap(),
            ));
            // A valid historical retained body is intentionally outside the live CE population.
            let old = tribute(0);
            tribute_writer.put(&old).unwrap();
            let retained = RetainedTributeReader::new(projection.clone());
            projection
                .apply_atomic(
                    &retained
                        .plan_retain_current(
                            RetainedTributePin {
                                input_lease_id: B256::repeat_byte(9),
                                worldwide_day: old.worldwide_day,
                            },
                            old.tribute_id,
                        )
                        .unwrap(),
                )
                .unwrap();
            tribute_writer.delete(old.tribute_id).unwrap();
            let mutations: Vec<_> = bodies
                .into_iter()
                .map(|(entity, id, stored)| FinalLeafMutation {
                    entity,
                    final_leaf: Some(
                        body_commitment(
                            ACTIVE_COMMITMENT_SCHEME,
                            stored.schema_version(),
                            id,
                            stored.payload(),
                        )
                        .unwrap(),
                    ),
                })
                .collect();
            let parent = MdbxAuthenticatedTree::open(
                ce.clone(),
                ExactParentIdentity {
                    commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                    block_number: 0,
                    block_hash: genesis_hash,
                    root: genesis.new_root,
                },
            )
            .unwrap();
            let prepared = parent.prepare_seal(1, &mutations, &[]).unwrap();
            header = OutbeHeader::new(Header {
                number: 1,
                parent_hash: genesis_hash,
                extra_data: encode_outbe_block_artifacts(&OutbeBlockArtifacts {
                    compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                        r_sealed: prepared.new_root(),
                    }),
                    ..Default::default()
                })
                .unwrap(),
                ..Default::default()
            });
            ce.apply_finalized(&prepared.freeze(header.hash_slow()))
                .unwrap();
        }
        write_state(
            &projection,
            &layout,
            Some(ProjectionCheckpoint {
                block_number: header.inner.number,
                block_hash: header.hash_slow(),
            }),
        );
        drop(projection);
        drop(ce);
        let ce = CeMdbxReadOnly::open(&layout.chain_root, identity).unwrap();
        Self {
            source,
            layout,
            ce,
            header,
            bucket_id,
        }
    }

    fn mutate(&self, change: impl FnOnce(Arc<RocksDbStorage>)) {
        change(Arc::new(
            RocksDbStorage::open(&self.layout.projection.as_ref().unwrap().root).unwrap(),
        ));
    }

    fn verify(&self) -> eyre::Result<(CeAuditReport, ProjectionBodyReport, u64)> {
        let scratch = tempfile::tempdir()?;
        let view = ProjectionBodyView::open(&self.layout, scratch.path())?;
        let work = CeAuditWork::create(
            scratch.path().join("audit"),
            CeAuditLimits {
                records_per_run: 2,
                merge_fan_in: 2,
            },
        )?;
        let mut expected = CeBodyAudit::create(&work)?;
        let ce_report = verify_ce(&self.ce, &self.header, &work, &mut expected)?;
        let body_report = view.verify_bodies(&self.ce.marker()?, expected, &work)?;
        let mut retained = CountRetained(0);
        view.audit_retained(&work, &mut retained)?;
        Ok((ce_report, body_report, retained.0))
    }
}

fn write_state(
    storage: &RocksDbStorage,
    layout: &RequestedLayout,
    checkpoint: Option<ProjectionCheckpoint>,
) {
    let state = ProjectionState {
        chain_id: layout.chain.chain().id(),
        genesis_hash: layout.chain.genesis_hash(),
        storage_schema_version: STORAGE_SCHEMA_VERSION,
        start_block: layout.projection.as_ref().unwrap().start_block,
        checkpoint,
    };
    storage
        .put(
            Namespace::new("projection_state").unwrap(),
            &Key::new(b"offchain_data".to_vec()).unwrap(),
            &Value::new(postcard::to_stdvec(&state).unwrap()).unwrap(),
        )
        .unwrap();
}

struct CountRetained(u64);
impl RetainedTributeAuditVisitor for CountRetained {
    fn visit_retained(&mut self, entry: RetainedTributeAuditEntry) -> Result<(), CeAuditError> {
        assert_eq!(entry.reference.tribute_id, tribute(0).tribute_id);
        self.0 += 1;
        Ok(())
    }
}

#[test]
fn exact_native_population_matches_all_domains_and_separates_retained_bodies() {
    for populated in [false, true] {
        let fixture = Fixture::new(populated);
        let before = fingerprint(fixture.source.path());
        let (ce, bodies, retained) = fixture.verify().unwrap();
        assert_eq!(bodies.checkpoint.block_hash, fixture.header.hash_slow());
        assert_eq!(
            bodies.equality.unwrap().bodies,
            if populated { 4 } else { 0 }
        );
        assert_eq!(ce.leaves, if populated { 8 } else { 0 });
        assert_eq!(retained, u64::from(populated));
        assert_eq!(fingerprint(fixture.source.path()), before);
    }
}

#[test]
fn unequal_height_or_hash_preserves_independent_reports_and_incomplete_equality() {
    for same_height in [false, true] {
        let fixture = Fixture::new(true);
        let checkpoint = ProjectionCheckpoint {
            block_number: if same_height { 1 } else { 0 },
            block_hash: B256::repeat_byte(0x99),
        };
        fixture.mutate(|storage| write_state(&storage, &fixture.layout, Some(checkpoint)));
        let before = fingerprint(fixture.source.path());
        let (ce, bodies, retained) = fixture.verify().unwrap();
        assert_eq!(ce.leaves, 8);
        assert_eq!(bodies.checkpoint, checkpoint);
        assert!(bodies.equality.is_err());
        assert_eq!(retained, 1);
        assert_eq!(fingerprint(fixture.source.path()), before);
    }
}

#[test]
fn missing_extra_changed_primary_or_index_fails_without_source_mutation() {
    for mutation in ["bucket", "tribute", "index", "changed", "extra"] {
        let fixture = Fixture::new(true);
        fixture.mutate(|storage| {
            let writer = TributeRepositoryWriter::new(storage.clone(), storage.clone());
            match mutation {
                "bucket" => storage
                    .delete(
                        Namespace::new("nod_buckets").unwrap(),
                        &Key::new(fixture.bucket_id.to_vec()).unwrap(),
                    )
                    .unwrap(),
                "tribute" => writer.delete(tribute(1).tribute_id).unwrap(),
                "index" => storage
                    .delete(
                        Namespace::new("tributes_by_owner").unwrap(),
                        &Key::new(
                            [
                                tribute(1).owner.as_slice(),
                                tribute(1).tribute_id.as_slice(),
                            ]
                            .concat(),
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                "changed" => {
                    let mut body = tribute(1);
                    body.issuance_amount_minor += U256::from(1);
                    writer.put(&body).unwrap();
                }
                "extra" => writer.put(&tribute(7)).unwrap(),
                _ => unreachable!(),
            }
        });
        let before = fingerprint(fixture.source.path());
        assert!(fixture.verify().is_err(), "{mutation}");
        assert_eq!(fingerprint(fixture.source.path()), before);
    }
}

#[test]
fn absent_projection_configuration_state_or_checkpoint_is_incomplete() {
    for missing in ["configuration", "state", "checkpoint"] {
        let mut fixture = Fixture::new(false);
        if missing == "configuration" {
            fixture.layout.projection = None;
        } else {
            fixture.mutate(|storage| {
                if missing == "state" {
                    storage
                        .delete(
                            Namespace::new("projection_state").unwrap(),
                            &Key::new(b"offchain_data".to_vec()).unwrap(),
                        )
                        .unwrap();
                } else {
                    write_state(&storage, &fixture.layout, None);
                }
            });
        }
        let before = fingerprint(fixture.source.path());
        let scratch = tempfile::tempdir().unwrap();
        let error = ProjectionBodyView::open(&fixture.layout, scratch.path())
            .err()
            .unwrap();
        assert!(
            error.downcast_ref::<Incomplete>().is_some(),
            "{missing}: {error}"
        );
        assert_eq!(fingerprint(fixture.source.path()), before);
        assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 0);
    }
}

#[test]
fn overlapping_scratch_is_rejected_before_creating_files() {
    let fixture = Fixture::new(false);
    let before = fingerprint(fixture.source.path());
    for root in [
        &fixture.layout.chain_root,
        &fixture.layout.consensus_root,
        &fixture.layout.ocomp_root,
        &fixture.layout.static_files_root,
        &fixture.layout.execution_rocksdb_root,
        &fixture.layout.projection.as_ref().unwrap().root,
    ] {
        assert!(
            ProjectionBodyView::open(&fixture.layout, &root.join("new-audit-scratch")).is_err()
        );
    }
    assert!(ProjectionBodyView::open(
        &fixture.layout,
        &fixture.source.path().join("configuration")
    )
    .is_err());
    assert_eq!(fingerprint(fixture.source.path()), before);
}

#[test]
fn missing_projection_root_or_current_is_incomplete_without_creating_a_view() {
    for missing_root in [false, true] {
        let fixture = Fixture::new(false);
        let projection_root = &fixture.layout.projection.as_ref().unwrap().root;
        if missing_root {
            fs::remove_dir_all(projection_root).unwrap();
        } else {
            fs::remove_file(projection_root.join("CURRENT")).unwrap();
        }
        let before = fingerprint(fixture.source.path());
        let scratch = tempfile::tempdir().unwrap();
        let error = ProjectionBodyView::open(&fixture.layout, scratch.path())
            .err()
            .unwrap();
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error}");
        assert!(!projection_root.join("CURRENT").exists());
        assert_eq!(projection_root.exists(), !missing_root);
        assert_eq!(fingerprint(fixture.source.path()), before);
        assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 0);
    }
}

#[test]
fn unequal_frontiers_do_not_skip_primary_or_index_corruption() {
    for malformed_bucket in [false, true] {
        let fixture = Fixture::new(true);
        fixture.mutate(|storage| {
            write_state(
                &storage,
                &fixture.layout,
                Some(ProjectionCheckpoint {
                    block_number: 0,
                    block_hash: fixture.layout.chain.genesis_hash(),
                }),
            );
            if malformed_bucket {
                storage
                    .put(
                        Namespace::new("nod_buckets").unwrap(),
                        &Key::new(fixture.bucket_id.to_vec()).unwrap(),
                        &Value::new(vec![0xff]).unwrap(),
                    )
                    .unwrap();
            } else {
                let body = tribute(1);
                storage
                    .delete(
                        Namespace::new("tributes_by_owner").unwrap(),
                        &Key::new([body.owner.as_slice(), body.tribute_id.as_slice()].concat())
                            .unwrap(),
                    )
                    .unwrap();
            }
        });
        let before = fingerprint(fixture.source.path());
        let error = fixture.verify().unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error}");
        assert_eq!(fingerprint(fixture.source.path()), before);
    }
}
