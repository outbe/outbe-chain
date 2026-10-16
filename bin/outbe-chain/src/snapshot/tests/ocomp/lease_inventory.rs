use crate::snapshot::{
    native::{HistoricalStateReader, RethReadOnlyView},
    tests::{headers::fingerprint, native_state},
    validation::{ocomp::verify_lease_inputs, Incomplete},
};
use alloy_consensus::Sealable;
use alloy_primitives::{Address, B256, U256};
use outbe_ocomp::control::poc_schema_limits;
use outbe_ocomp_protocol::{
    intent::{
        ActivationPreconditionsV1, DayType, FrozenMetadosisValuesV1, JobIntentV1,
        MetadosisAttemptPreconditionV1, MetadosisExpectedStatus, NodTargetPreconditionV1,
        TributeInputBindingV1,
    },
    state::{OcompFinalizedJobV1, OcompJobRecordV1, OcompJobStatus},
};
use outbe_primitives::{
    addresses::TRIBUTE_ADDRESS,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::WorldwideDay,
    tribute_encryption::TributeContextV2,
    wwd_entity_id::derive_poseidon_entity_id,
    OutbeHeader,
};
use outbe_tribute::{
    storage_records::{StoredTribute, StoredTributeEntryExt},
    TributeContract,
};
use reth_ethereum::provider::db::{
    database::Database,
    init_db,
    mdbx::DatabaseArguments,
    tables,
    transaction::{DbTx, DbTxMut},
};
use reth_primitives_traits::StorageEntry;

const DAY: WorldwideDay = WorldwideDay::new(20_260_901);
fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn intent(count: u32, nominal: U256) -> JobIntentV1 {
    let day = DAY.value();
    let intent = JobIntentV1 {
        chain_id: 54322345,
        genesis_hash: hash(1),
        fork_id: hash(2),
        wwd: day,
        pending_nonce: 0,
        attempt: 0,
        protocol_bundle_hash: hash(3),
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
                exact_count: count,
                exact_nominal_total: nominal,
            },
            nod: NodTargetPreconditionV1 {
                wwd: day,
                target_generation: 1,
                namespace_root_before: hash(9),
                max_nod_count: count,
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

#[derive(Clone, Copy, Debug)]
enum Fault {
    None,
    MissingRecord,
    DuplicateIndex,
    WrongOwner,
    MissingHistory,
    WrongHistory,
}

struct Fixture {
    root: tempfile::TempDir,
    layout: crate::snapshot::config::NativeLayout,
    job: OcompJobRecordV1,
}

impl Fixture {
    fn new(version: u32, fault: Fault) -> Self {
        let (root, layout, _) = crate::snapshot::tests::evm::state_fixture(version);
        let mut owner = HashMapStorageProvider::new(layout.chain.chain().id());
        let (record_slot, first_id, owner_slot, second_index_slot) =
            StorageHandle::enter(&mut owner, |storage| {
                let contract = TributeContract::new(storage);
                let mut first = None;
                // The physical day index is deliberately not sorted by owner.
                for index in [5u8, 1, 4, 2, 3] {
                    let address = Address::repeat_byte(index);
                    let body = native_state::encrypted(TributeContextV2 {
                        chain_id: layout.chain.chain().id(),
                        tribute_id: derive_poseidon_entity_id(address, DAY).unwrap(),
                        owner: address,
                        worldwide_day: DAY,
                        issuance_currency: 840,
                        reference_currency: 840,
                        tribute_price_minor: U256::from(2),
                        offer_input_hash: hash(index),
                    });
                    contract
                        .insert_state_record(&StoredTribute::from(&body))
                        .unwrap();
                    if first.is_none() {
                        first = Some(body);
                    }
                }
                let first = first.unwrap();
                let entry = contract.records.entry(first.context.tribute_id);
                let record_slot = entry
                    .encrypted_amounts()
                    .encoded_words(&first.encrypted_amounts)
                    .last()
                    .unwrap()
                    .0;
                (
                    record_slot,
                    first.context.tribute_id,
                    entry.owner().slot(),
                    contract.day_ids.list(&DAY).data_slot(1),
                )
            });
        match fault {
            Fault::MissingRecord => {
                owner.storage.remove(&(TRIBUTE_ADDRESS, owner_slot));
            }
            Fault::DuplicateIndex => {
                owner.storage.insert(
                    (TRIBUTE_ADDRESS, second_index_slot),
                    U256::from_be_slice(first_id.as_slice()),
                );
            }
            Fault::WrongOwner => {
                owner
                    .storage
                    .insert((TRIBUTE_ADDRESS, owner_slot), U256::from(0x99));
            }
            _ => {}
        }
        let before = native_state::from_owner(&owner);
        let mut after = before.clone();
        let key = B256::from(record_slot.to_be_bytes::<32>());
        let old = after
            .get_mut(&TRIBUTE_ADDRESS)
            .unwrap()
            .1
            .get_mut(&key)
            .unwrap();
        *old ^= U256::ONE;
        native_state::persist(&layout, version, &before, &after, 101);
        let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        let mut request = tx
            .get::<tables::Headers<OutbeHeader>>(100)
            .unwrap()
            .unwrap();
        request.inner.state_root = native_state::root(&before);
        let mut execution = tx
            .get::<tables::Headers<OutbeHeader>>(101)
            .unwrap()
            .unwrap();
        execution.inner.parent_hash = request.hash_slow();
        execution.inner.state_root = native_state::root(&after);
        for header in [request, execution] {
            tx.put::<tables::CanonicalHeaders>(header.inner.number, header.hash_slow())
                .unwrap();
            tx.put::<tables::HeaderNumbers>(header.hash_slow(), header.inner.number)
                .unwrap();
            tx.put::<tables::Headers<OutbeHeader>>(header.inner.number, header)
                .unwrap();
        }
        match fault {
            Fault::MissingHistory => {
                tx.delete::<tables::StorageChangeSets>((101, TRIBUTE_ADDRESS).into(), None)
                    .unwrap();
            }
            Fault::WrongHistory => {
                tx.delete::<tables::StorageChangeSets>((101, TRIBUTE_ADDRESS).into(), None)
                    .unwrap();
                tx.put::<tables::StorageChangeSets>(
                    (101, TRIBUTE_ADDRESS).into(),
                    StorageEntry {
                        key,
                        value: U256::ONE,
                    },
                )
                .unwrap();
            }
            _ => {}
        }
        tx.commit().unwrap();
        drop(db);
        native_state::publish_history(&layout);
        let job = OcompJobRecordV1 {
            intent: intent(5, U256::from(3500)),
            intent_height: 100,
            status: OcompJobStatus::AwaitingFinality,
            finalized: None,
            terminal: None,
        };
        Self { root, layout, job }
    }

    fn check(&self, job: &OcompJobRecordV1, maximum: Option<u64>) -> eyre::Result<()> {
        let before = fingerprint(self.root.path());
        let scratch = tempfile::tempdir().unwrap();
        let result = (|| {
            let view = RethReadOnlyView::open(&self.layout)?;
            let history = HistoricalStateReader::open(&self.layout, &view, scratch.path())?;
            verify_lease_inputs(&history, &view, job, maximum)
        })();
        assert_eq!(fingerprint(self.root.path()), before);
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
        result
    }
}

#[test]
fn complete_native_request_population_uses_history_instead_of_current_ciphertext() {
    for version in [1, 2] {
        let fixture = Fixture::new(version, Fault::None);
        fixture.check(&fixture.job, Some(5)).unwrap();
    }
}

#[test]
fn missing_record_duplicate_index_or_wrong_identity_cannot_close_native_coverage() {
    for version in [1, 2] {
        for fault in [
            Fault::MissingRecord,
            Fault::DuplicateIndex,
            Fault::WrongOwner,
        ] {
            let fixture = Fixture::new(version, fault);
            assert!(
                fixture.check(&fixture.job, None).is_err(),
                "{version}: {fault:?}"
            );
        }
    }
}

#[test]
fn missing_or_tampered_before_values_cannot_substitute_current_state() {
    for version in [1, 2] {
        for fault in [Fault::MissingHistory, Fault::WrongHistory] {
            let fixture = Fixture::new(version, fault);
            assert!(
                fixture.check(&fixture.job, None).is_err(),
                "{version}: {fault:?}"
            );
        }
    }
}

#[test]
fn native_day_count_and_checkpoint_bind_exactly() {
    for version in [1, 2] {
        let fixture = Fixture::new(version, Fault::None);
        for count in [4, 6] {
            let mut job = fixture.job.clone();
            job.intent = intent(count, U256::from(3500));
            assert!(fixture.check(&job, None).is_err());
        }
        let request = RethReadOnlyView::open(&fixture.layout)
            .unwrap()
            .header(100)
            .unwrap()
            .unwrap();
        for wrong_hash in [false, true] {
            let mut job = fixture.job.clone();
            job.finalized = Some(OcompFinalizedJobV1 {
                job_id: hash(0x81),
                finalized_request_block_hash: if wrong_hash {
                    hash(0x82)
                } else {
                    request.hash_slow()
                },
                finalized_request_state_root: if wrong_hash {
                    request.inner.state_root
                } else {
                    hash(0x84)
                },
                finality_recorded_height: 101,
                open_height: 105,
                deadline_height: 200,
                quorum: None,
            });
            assert!(fixture.check(&job, None).is_err());
        }
    }
}

#[test]
fn source_integrity_does_not_decrypt_or_sum_private_amounts() {
    let fixture = Fixture::new(2, Fault::None);
    for nominal in [3499u64, 3501] {
        let mut job = fixture.job.clone();
        job.intent = intent(5, U256::from(nominal));
        fixture.check(&job, None).unwrap();
    }
}

#[test]
fn native_scan_budget_is_bounded_and_exact_budget_finishes() {
    let fixture = Fixture::new(2, Fault::None);
    for maximum in [0, 1, 4] {
        let error = fixture.check(&fixture.job, Some(maximum)).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
    }
    fixture.check(&fixture.job, Some(5)).unwrap();
}

#[test]
fn overlapping_history_scratch_is_rejected_without_source_mutation() {
    let fixture = Fixture::new(2, Fault::None);
    let view = RethReadOnlyView::open(&fixture.layout).unwrap();
    let before = fingerprint(fixture.root.path());
    assert!(HistoricalStateReader::open(
        &fixture.layout,
        &view,
        &fixture.layout.chain_root.join("db")
    )
    .is_err());
    assert_eq!(fingerprint(fixture.root.path()), before);
}
