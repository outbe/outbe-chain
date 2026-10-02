use crate::snapshot::tests::projection_fixture::PartitionFixtureReader as RocksDbReader;
use crate::snapshot::tests::projection_fixture::PartitionFixtureStore as RocksDbStorage;
use crate::snapshot::{
    tests::headers::fingerprint,
    validation::{ocomp::verify_lease_inputs, Incomplete},
};
use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    body_commitment, derive_poseidon_entity_id, encode_tribute_v1, partition_collection_key,
    tribute_partition_root_from_leaves, PartitionRef, StoredBody, ACTIVE_COMMITMENT_SCHEME,
    BODY_SCHEMA_V1,
};
use outbe_ocomp::{control::poc_schema_limits, exporter::TributeStreamSummary};
use outbe_ocomp_protocol::intent::{
    ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType, FrozenMetadosisValuesV1,
    JobIntentV1, MetadosisAttemptPreconditionV1, MetadosisExpectedStatus, NodTargetPreconditionV1,
    TributeInputBindingV1,
};
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, Key, Namespace, StorageReaderHandle,
    StorageWriterHandle, Value,
};
use outbe_primitives::time::WorldwideDay;
use outbe_snapshot::layout::ProtectedPaths;
use outbe_tribute::{
    canonical_body, RetainedTributePin, RetainedTributeReader, TributeData,
    TributeRepositoryReader, TributeRepositoryWriter, OCOMP_RETAINED_TRIBUTES_NAMESPACE,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

const DAY: WorldwideDay = WorldwideDay::new(20_260_901);
fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn intent(collection_root: B256, count: u32, nominal: U256) -> JobIntentV1 {
    let day = DAY.value();
    let collection_key = B256::from(
        *partition_collection_key(PartitionRef::TributeWwd(DAY))
            .unwrap()
            .1
            .as_bytes(),
    );
    let intent = JobIntentV1 {
        chain_id: 54322345,
        genesis_hash: hash(1),
        fork_id: hash(2),
        wwd: day,
        pending_nonce: 0,
        attempt: 0,
        protocol_bundle_hash: hash(3),
        ce_sealed_root: hash(5),
        sealed_tribute_collection_key: collection_key,
        sealed_tribute_collection_root: collection_root,
        authenticated_day_count: count,
        authenticated_day_nominal: nominal,
        pre_admission_envelope_hash: hash(6),
        source_availability_policy_id: hash(7),
        frozen_metadosis_values: FrozenMetadosisValuesV1 {
            day_type: DayType::Green,
            day_limit: nominal,
            previous_vwap: nominal,
            current_vwap: nominal,
            gratis_demand: U256::ZERO,
            day_gratis_limit_minor: U256::ZERO,
            lysis_limit_minor: nominal,
            desis_limit_minor: U256::ZERO,
            request_limit_split_receipt_hash: hash(8),
        },
        logical_evaluation_height: 100,
        logical_evaluation_time: 1000,
        activation_preconditions: ActivationPreconditionsV1 {
            tribute: TributeInputBindingV1 {
                wwd: day,
                source_generation: 1,
                collection_key,
                sealed_collection_root: collection_root,
                exact_count: count,
                exact_nominal_total: nominal,
            },
            nod: NodTargetPreconditionV1 {
                wwd: day,
                target_generation: 1,
                namespace_root_before: hash(9),
                max_nod_count: count,
            },
            contributors: ContributorTargetPreconditionV1 {
                worldwide_day: day,
                expected_series_version: 1,
                max_contributor_count: count,
                max_eligible_nominal_total: nominal,
            },
            metadosis: MetadosisAttemptPreconditionV1 {
                wwd: day,
                pending_nonce: 0,
                expected_status: MetadosisExpectedStatus::OffchainPending,
                state_version: 1,
            },
        },
        result_validator_set_epoch: 1,
        result_committee_set_hash: hash(10),
        result_ocomp_binding_hash: hash(11),
        result_member_count: 4,
        result_quorum_threshold: 3,
        custody_committee_epoch_hash: None,
    };
    intent.encode_canonical(&poc_schema_limits()).unwrap();
    intent
}

fn body(index: u8, day: WorldwideDay) -> TributeData {
    let owner = Address::repeat_byte(index);
    TributeData {
        tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
        owner,
        worldwide_day: day,
        issuance_amount_minor: U256::from(1000),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(700),
        reference_currency: 840,
        tribute_price_minor: U256::from(2),
        exclude_from_intex_issuance: false,
    }
}

#[derive(Clone, Copy)]
enum Placement {
    Live,
    Retained,
    Union,
    Absent,
    OtherLease,
    OtherDay,
}
#[derive(Clone, Copy)]
enum Fault {
    None,
    MissingBody,
    CorruptBody,
    CrossDayBody,
}

struct Fixture {
    reader: StorageReaderHandle,
    root: tempfile::TempDir,
    source: PathBuf,
    scratch: PathBuf,
    intent: JobIntentV1,
    exact_body_bytes: u64,
}

impl Fixture {
    fn new(placement: Placement, fault: Fault) -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("primary");
        let scratch = root.path().join("scratch");
        fs::create_dir(&scratch).unwrap();
        let bodies: Vec<_> = (1..=5).map(|index| body(index, DAY)).collect();
        let mut exact_body_bytes = 0;
        let leaves: Vec<_> = bodies
            .iter()
            .map(|body| {
                let bytes = encode_tribute_v1(&canonical_body(body)).unwrap();
                exact_body_bytes += bytes.len() as u64;
                (
                    body.tribute_id,
                    body_commitment(
                        ACTIVE_COMMITMENT_SCHEME,
                        BODY_SCHEMA_V1,
                        body.tribute_id,
                        &bytes,
                    )
                    .unwrap(),
                )
            })
            .collect();
        let collection_root = tribute_partition_root_from_leaves(DAY, leaves).unwrap();
        let intent = intent(collection_root, 5, U256::from(3500));
        {
            let storage = Arc::new(RocksDbStorage::open(&source).unwrap());
            let reader: StorageReaderHandle = storage.clone();
            let writer: StorageWriterHandle = storage.clone();
            let current = TributeRepositoryReader::new(reader.clone());
            let repository = TributeRepositoryWriter::new(reader.clone(), writer.clone());
            let retained = RetainedTributeReader::new(reader);
            let pin = RetainedTributePin {
                input_lease_id: if matches!(placement, Placement::OtherLease) {
                    hash(0xee)
                } else {
                    intent.input_lease_id().unwrap()
                },
                worldwide_day: DAY,
            };
            for (index, data) in bodies.iter().enumerate() {
                if matches!(placement, Placement::Absent) {
                    continue;
                }
                if matches!(placement, Placement::OtherDay) {
                    repository
                        .put(&body(index as u8 + 1, WorldwideDay::new(20_260_902)))
                        .unwrap();
                    continue;
                }
                repository.put(data).unwrap();
                let retain = matches!(placement, Placement::Retained | Placement::OtherLease)
                    || matches!(placement, Placement::Union) && index <= 2;
                if retain {
                    let retained_batch =
                        retained.plan_retain_current(pin, data.tribute_id).unwrap();
                    let mut batch = AtomicWriteBatch::new();
                    batch.extend(retained_batch.operations().iter().cloned());
                    if !matches!(placement, Placement::Union) || index < 2 {
                        batch.extend(
                            current
                                .projection_session(&[data.tribute_id])
                                .unwrap()
                                .delete(data.tribute_id)
                                .unwrap()
                                .operations()
                                .iter()
                                .cloned(),
                        );
                    }
                    writer.apply_atomic(&batch).unwrap();
                    if index == 0 && matches!(fault, Fault::MissingBody) {
                        // Remove only the native body put, leaving its real retained index.
                        for operation in retained_batch.operations() {
                            if let AtomicWriteOperation::Put { namespace, key, .. } = operation {
                                if namespace.as_str() == OCOMP_RETAINED_TRIBUTES_NAMESPACE {
                                    writer.delete(namespace.clone(), key).unwrap();
                                }
                            }
                        }
                    }
                } else if index == 0 {
                    let namespace = Namespace::new("tributes").unwrap();
                    let key = Key::new(data.tribute_id.to_vec()).unwrap();
                    match fault {
                        Fault::None => {}
                        Fault::MissingBody => writer.delete(namespace, &key).unwrap(),
                        Fault::CorruptBody => writer
                            .put(
                                namespace,
                                &key,
                                &Value::new(b"invalid stored body".to_vec()).unwrap(),
                            )
                            .unwrap(),
                        Fault::CrossDayBody => {
                            // A valid foreign-day body under the selected native primary key.
                            let bytes = encode_tribute_v1(&canonical_body(&body(
                                1,
                                WorldwideDay::new(20_260_902),
                            )))
                            .unwrap();
                            let value =
                                Value::new(StoredBody::new_v1(bytes).unwrap().encode()).unwrap();
                            writer.put(namespace, &key, &value).unwrap();
                        }
                    }
                }
            }
        }
        // The primary writer is stopped. Session creation may write secondary
        // metadata, so open it before the measured read-only adapter call.
        let reader: StorageReaderHandle =
            Arc::new(RocksDbReader::open(&source, &root.path().join("secondary")).unwrap());
        Self {
            reader,
            root,
            source,
            scratch,
            intent,
            exact_body_bytes,
        }
    }

    fn check(
        &self,
        authority: &JobIntentV1,
        maximum: Option<u64>,
    ) -> eyre::Result<TributeStreamSummary> {
        self.check_at(authority, &self.scratch, maximum)
    }

    fn check_at(
        &self,
        authority: &JobIntentV1,
        scratch: &Path,
        maximum: Option<u64>,
    ) -> eyre::Result<TributeStreamSummary> {
        let before = fingerprint(&self.source);
        let scratch_before = fingerprint(scratch);
        let result = verify_lease_inputs(
            self.reader.clone(),
            authority,
            scratch,
            &ProtectedPaths(vec![self.source.clone()]),
            maximum,
        );
        assert_eq!(fingerprint(&self.source), before);
        assert_eq!(
            fingerprint(scratch),
            scratch_before,
            "temporary verification files must be cleaned on every exit"
        );
        result
    }
}

#[test]
fn live_retained_and_deduplicated_union_close_actual_native_root_and_nominal() {
    for placement in [Placement::Live, Placement::Retained, Placement::Union] {
        let fixture = Fixture::new(placement, Fault::None);
        let summary = fixture.check(&fixture.intent, Some(5)).unwrap();
        assert_eq!(
            summary,
            TributeStreamSummary {
                record_count: 5,
                nominal_total: U256::from(3500),
                exact_body_bytes: fixture.exact_body_bytes
            }
        );
    }
}

#[test]
fn absent_required_partition_and_missing_selected_live_or_retained_body_are_incomplete() {
    for (placement, fault) in [
        (Placement::Absent, Fault::None),
        (Placement::Live, Fault::MissingBody),
        (Placement::Retained, Fault::MissingBody),
    ] {
        let fixture = Fixture::new(placement, fault);
        let error = fixture.check(&fixture.intent, None).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
    }
}

#[test]
fn bodies_under_other_lease_or_day_do_not_substitute_required_partition() {
    for placement in [Placement::OtherLease, Placement::OtherDay] {
        let fixture = Fixture::new(placement, Fault::None);
        let error = fixture.check(&fixture.intent, None).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
    }
}

#[test]
fn corrupt_or_foreign_day_body_under_selected_key_is_failed() {
    for fault in [Fault::CorruptBody, Fault::CrossDayBody] {
        let fixture = Fixture::new(Placement::Live, fault);
        let error = fixture.check(&fixture.intent, None).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
    }
}

#[test]
fn full_count_wrong_root_count_overrun_and_nominal_contradictions_are_failed() {
    let fixture = Fixture::new(Placement::Live, Fault::None);
    for authority in [
        intent(hash(0x99), 5, U256::from(3500)),
        intent(
            fixture.intent.sealed_tribute_collection_root,
            4,
            U256::from(3500),
        ),
        intent(
            fixture.intent.sealed_tribute_collection_root,
            5,
            U256::from(3499),
        ),
        intent(
            fixture.intent.sealed_tribute_collection_root,
            5,
            U256::from(3501),
        ),
    ] {
        let error = fixture.check(&authority, None).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
    }
    let incomplete = intent(
        fixture.intent.sealed_tribute_collection_root,
        6,
        U256::from(4200),
    );
    let error = fixture.check(&incomplete, None).unwrap_err();
    assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
}

#[test]
fn resource_limit_reports_incomplete_and_exact_budget_can_finish() {
    let fixture = Fixture::new(Placement::Union, Fault::None);
    for maximum in [0, 1, 4] {
        let error = fixture.check(&fixture.intent, Some(maximum)).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
    }
    assert_eq!(
        fixture
            .check(&fixture.intent, Some(5))
            .unwrap()
            .record_count,
        5
    );
}

#[test]
fn overlapping_scratch_is_rejected_before_source_mutation() {
    let fixture = Fixture::new(Placement::Union, Fault::None);
    let error = fixture
        .check_at(&fixture.intent, &fixture.source, None)
        .unwrap_err();
    assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
    let missing = fixture.source.join("new-scratch");
    let before = fingerprint(&fixture.source);
    let error = verify_lease_inputs(
        fixture.reader.clone(),
        &fixture.intent,
        &missing,
        &ProtectedPaths(vec![fixture.source.clone()]),
        None,
    )
    .unwrap_err();
    assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
    assert!(!missing.exists());
    assert_eq!(fingerprint(&fixture.source), before);
    assert!(fixture.root.path().is_dir());
}
