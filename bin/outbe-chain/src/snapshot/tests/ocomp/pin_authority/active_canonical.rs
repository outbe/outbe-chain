mod exported_composition;

fn source_body() -> outbe_tribute::TributeData {
    let owner = alloy_primitives::Address::repeat_byte(1);
    outbe_tribute::TributeData {
        tribute_id: outbe_primitives::wwd_entity_id::derive_poseidon_entity_id(owner, DAY).unwrap(),
        owner,
        worldwide_day: DAY,
        issuance_amount_minor: U256::from(1000),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(700),
        reference_currency: 840,
        tribute_price_minor: U256::from(2),
    }
}

fn bind_source(intent: &mut JobIntentV1) {
    let body = source_body();
    intent.authenticated_day_count = 1;
    intent.authenticated_day_nominal = body.nominal_amount_minor;
    let tribute = &mut intent.activation_preconditions.tribute;
    tribute.exact_count = 1;
    tribute.exact_nominal_total = body.nominal_amount_minor;
}

fn encrypted_source(chain_id: u64) -> outbe_primitives::tribute_encryption::EncryptedTributeV2 {
    let body = source_body();
    crate::snapshot::tests::native_state::encrypted(
        outbe_primitives::tribute_encryption::TributeContextV2 {
            chain_id,
            tribute_id: body.tribute_id,
            owner: body.owner,
            worldwide_day: DAY,
            issuance_currency: body.issuance_currency,
            reference_currency: body.reference_currency,
            tribute_price_minor: body.tribute_price_minor,
            offer_input_hash: alloy_primitives::B256::repeat_byte(0x43),
        },
    )
}

fn source_state(chain_id: u64) -> crate::snapshot::tests::native_state::State {
    let encrypted = encrypted_source(chain_id);
    let mut owner = HashMapStorageProvider::new(chain_id);
    StorageHandle::enter(&mut owner, |storage| {
        outbe_tribute::TributeContract::new(storage)
            .insert_state_record(&outbe_tribute::storage_records::StoredTribute::from(
                &encrypted,
            ))
            .unwrap();
    });
    crate::snapshot::tests::native_state::from_owner(&owner)
}

fn source_ciphertext_slot(chain_id: u64) -> alloy_primitives::B256 {
    use outbe_tribute::storage_records::StoredTributeEntryExt;
    let encrypted = encrypted_source(chain_id);
    let mut owner = HashMapStorageProvider::new(chain_id);
    StorageHandle::enter(&mut owner, |storage| {
        let contract = outbe_tribute::TributeContract::new(storage);
        let words = contract
            .records
            .entry(encrypted.context.tribute_id)
            .encrypted_amounts()
            .encoded_words(&encrypted.encrypted_amounts);
        alloy_primitives::B256::from(words.last().unwrap().0.to_be_bytes::<32>())
    })
}

fn write_source(layout: &crate::snapshot::config::RequestedLayout) {
    use crate::snapshot::tests::native_state;
    use reth_ethereum::provider::db::{
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        tables,
        transaction::{DbTx, DbTxMut},
    };
    let source = source_state(layout.chain.chain().id());
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx().unwrap();
    let settings = tx
        .get::<tables::Metadata>("storage_settings".into())
        .unwrap()
        .map(|bytes| serde_json::from_slice::<reth_provider::StorageSettings>(&bytes).unwrap())
        .unwrap_or_else(reth_provider::StorageSettings::v1);
    let mut request = tx
        .get::<tables::Headers<OutbeHeader>>(100)
        .unwrap()
        .unwrap();
    let mut next = tx
        .get::<tables::Headers<OutbeHeader>>(101)
        .unwrap()
        .unwrap();
    drop(tx);
    drop(db);
    native_state::persist(
        layout,
        if settings.is_v2() { 2 } else { 1 },
        &source,
        &source,
        102,
    );
    request.inner.state_root = native_state::root(&source);
    next.inner.state_root = request.inner.state_root;
    next.inner.parent_hash = request.hash_slow();
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    for header in [request, next] {
        tx.put::<tables::CanonicalHeaders>(header.inner.number, header.hash_slow())
            .unwrap();
        tx.put::<tables::HeaderNumbers>(header.hash_slow(), header.inner.number)
            .unwrap();
        tx.put::<tables::Headers<OutbeHeader>>(header.inner.number, header)
            .unwrap();
    }
    tx.commit().unwrap();
}

#[test]
fn source_complete_active_after_closure_passes_without_local_pin_or_export() {
    use crate::snapshot::validation::ocomp::{
        verify_canonical_obligations, CanonicalLocalPinStage,
    };
    use reth_ethereum::provider::db::{
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        models::StoredBlockBodyIndices,
        tables,
        transaction::{DbTx, DbTxMut},
    };
    for version in [1, 2] {
        let request = std::cell::RefCell::new(None);
        super::super::with_canonical_frontiers(
            version,
            |layout| {
                write_source(layout);
                let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
                let tx = db.tx_mut().unwrap();
                *request.borrow_mut() = Some(
                    tx.get::<tables::Headers<OutbeHeader>>(101)
                        .unwrap()
                        .unwrap(),
                );
                tx.put::<tables::BlockBodyIndices>(
                    101,
                    StoredBlockBodyIndices {
                        first_tx_num: 0,
                        tx_count: 0,
                    },
                )
                .unwrap();
                tx.commit().unwrap();
            },
            |_| {
                fixture(
                    request.borrow().as_ref().unwrap(),
                    Phase::AwaitingFinality,
                    bind_source,
                )
                .owner
            },
            |state, source, layout, scratch| {
                let expected = fixture(
                    request.borrow().as_ref().unwrap(),
                    Phase::AwaitingFinality,
                    bind_source,
                )
                .job;
                let audit =
                    verify_canonical_obligations(state, source, (layout, scratch), None, None)
                        .unwrap();

                assert_eq!(audit.closure.checkpoint.current.block_number, 100);
                assert_eq!(audit.bounds.active_intents, 1);
                assert_eq!(audit.active.len(), 1);
                let active = &audit.active[0];
                assert_eq!(
                    active.intent_id,
                    expected.intent.intent_id(&poc_schema_limits()).unwrap()
                );
                assert_eq!(active.job, expected);
                assert_eq!(active.job.intent_height, 101);
                assert_eq!(active.job.status, OcompJobStatus::AwaitingFinality);
                assert!(active.job.finalized.is_none());
                assert_eq!(active.pin_stage, CanonicalLocalPinStage::Absent);

                assert!(active.source_verified);
                assert!(!active.export_verified);
                assert!(audit.pins.is_empty());
                assert_eq!(audit.source_leases, 1);
                assert_eq!(audit.complete_exports, 0);
                assert_eq!(audit.input_chunks, 0);
                assert_eq!(audit.nod.jobs, 0);
                assert!(!layout.consensus_root.join("ocomp_retention").exists());
                assert!(!layout.ocomp_root.join("supervisor-v1/jobs").exists());
            },
        );
    }
}

#[test]
fn later_fifo_budget_preserves_discovered_active_identity_and_partial_scan_bounds() {
    use crate::snapshot::validation::{
        ocomp::verify_canonical_obligations,
        report::{CheckName, CheckStatus, ValidationReport},
        Incomplete,
    };
    for version in [1, 2] {
        super::super::with_canonical_frontiers(
            version,
            |_| {},
            |request| {
                let mut prepared = fixture(request, Phase::AwaitingFinality, |_| {});
                // Replace only the fixture's empty NOD inventory with two native
                // queued generations. Metadosis/Registry authority stays intact.
                prepared.owner.storage.extend(queued_owner(2).storage);
                prepared.owner
            },
            |state, source, layout, scratch| {
                let expected = state.live_ocomp_jobs().unwrap();
                assert_eq!(expected.len(), 1);
                let mut report = ValidationReport::new([CheckName::Ocomp]);
                let error = verify_canonical_obligations(
                    state,
                    source,
                    (layout, scratch),
                    Some(1),
                    Some(&mut report),
                )
                .err()
                .expect("the second FIFO entry exceeds the selected cap");
                assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
                assert!(
                    error.to_string().contains("NOD FIFO scan stopped at 1/2"),
                    "{error:#}"
                );
                assert_eq!(report.active_ocomp.len(), 1, "later inventory interruption must not erase the independently discovered active intent");
                let active = &report.active_ocomp[0];
                assert_eq!(active.intent_id, hex::encode(expected[0].0));
                assert_eq!(active.job_id, None);
                assert_eq!(active.request_height, expected[0].1.intent_height);
                assert_eq!(active.canonical_status, "AwaitingFinality");
                assert_eq!(active.pin_stage, "NotInspected");

                assert!(!active.source_verified);
                assert!(!active.export_verified);
                let active_bounds = report
                    .inventory_bounds
                    .iter()
                    .find(|b| b.name == "active_intents")
                    .expect("completed active inventory bound");
                assert_eq!(
                    (
                        active_bounds.start,
                        active_bounds.end_exclusive,
                        active_bounds.visited
                    ),
                    (0, 1, 1)
                );
                let fifo = report
                    .inventory_bounds
                    .iter()
                    .find(|b| b.name == "nod_fifo")
                    .expect("interrupted FIFO bound");
                assert_eq!((fifo.start, fifo.end_exclusive, fifo.visited), (1, 3, 1));

                assert!(report.observed.c_current.is_none());
                assert_eq!(
                    report.check(CheckName::Ocomp).status,
                    CheckStatus::Incomplete
                );
                assert!(!report.success());
            },
        );
    }
}

#[test]
fn active_report_preserves_identity_and_distinguishes_unverified_local_capabilities() {
    use crate::snapshot::validation::{
        ocomp::{CanonicalActiveAudit, CanonicalLocalPinStage},
        report::{ActiveOcompObservation, CheckName, ValidationReport},
    };
    with_prepared_owner_storage(
        1,
        400,
        |request| fixture(request, Phase::AwaitingFinality, |_| {}).owner,
        |state, source| {
            let (intent_id, job) = state.live_ocomp_jobs().unwrap().pop().unwrap();
            let request_height = job.intent_height;
            let day = job.intent.wwd;
            assert!(job.finalized.is_none());
            let audit = CanonicalActiveAudit {
                intent_id,
                job,
                pin_stage: CanonicalLocalPinStage::Absent,
                source_verified: false,
                export_verified: false,
            };
            let mut report = ValidationReport::new([CheckName::Ocomp]);
            report
                .active_ocomp
                .push(ActiveOcompObservation::from(&audit));
            let json = serde_json::to_value(report).unwrap();
            let observed = &json["active_ocomp"][0];
            assert_eq!(observed["intent_id"], hex::encode(intent_id));
            assert_eq!(observed["job_id"], serde_json::Value::Null);
            assert_eq!(observed["request_height"], request_height);
            assert_eq!(observed["worldwide_day"], day);
            assert_eq!(observed["canonical_status"], "AwaitingFinality");
            assert_eq!(observed["pin_stage"], "Absent");
            assert!(observed.get("projection_before_request").is_none());
            assert_eq!(observed["source_verified"], false);
            assert_eq!(observed["export_verified"], false);
            assert!(source.header(request_height).unwrap().is_some());
        },
    );
}

#[test]
fn full_canonical_composition_finds_active_obligation_without_any_local_pin_or_job() {
    use crate::snapshot::validation::{
        ocomp::verify_canonical_obligations,
        report::{CheckName, ValidationReport},
        Incomplete,
    };
    for version in [1, 2] {
        for phase in [Phase::AwaitingFinality, Phase::VotingOpen] {
            super::super::with_canonical_frontiers(
                version,
                |_| {},
                |request| fixture(request, phase, |_| {}).owner,
                |state, source, layout, scratch| {
                    assert!(!layout.consensus_root.join("ocomp_retention").exists());
                    assert!(!layout.ocomp_root.join("supervisor-v1/jobs").exists());
                    let mut report = ValidationReport::new([CheckName::Ocomp]);
                    let error = verify_canonical_obligations(
                        state,
                        source,
                        (layout, scratch),
                        None,
                        Some(&mut report),
                    )
                    .err()
                    .expect(
                        "a canonical active job with an inconsistent native day count cannot pass",
                    );
                    assert!(
                        error.downcast_ref::<Incomplete>().is_none(),
                        "{phase:?}: {error:#}"
                    );
                    assert!(
                        format!("{error:#}").contains("native Tribute day count mismatch"),
                        "must discover the active source count conflict: {error:#}"
                    );
                    let (intent_id, job) = state.live_ocomp_jobs().unwrap().pop().unwrap();
                    assert_eq!(report.active_ocomp.len(), 1);
                    let observation = &report.active_ocomp[0];
                    assert_eq!(observation.intent_id, hex::encode(intent_id));
                    assert_eq!(
                        observation.job_id,
                        job.finalized.as_ref().map(|f| hex::encode(f.job_id))
                    );
                    assert_eq!(observation.request_height, job.intent_height);
                    assert_eq!(observation.worldwide_day, job.intent.wwd);
                    assert_eq!(observation.pin_stage, "Absent");
                    assert!(!observation.source_verified);
                    assert!(!observation.export_verified);

                    assert_eq!(report.observed.c_current.as_ref().unwrap().number, 100);
                    assert!(report
                        .inventory_bounds
                        .iter()
                        .any(|b| b.name == "active_intents" && b.visited == 1));
                    assert!(!report.success());
                },
            );
        }
    }
}

#[test]
fn canonical_active_budget_is_incomplete_before_missing_local_inputs() {
    use crate::snapshot::validation::{ocomp::verify_canonical_obligations, Incomplete};
    super::super::with_canonical_frontiers(
        1,
        |_| {},
        |request| fixture(request, Phase::VotingOpen, |_| {}).owner,
        |state, source, layout, scratch| {
            let error =
                verify_canonical_obligations(state, source, (layout, scratch), Some(0), None)
                    .err()
                    .expect("active inventory cannot truncate to zero success");
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
            assert!(
                error.to_string().contains("active intent scan requires 1"),
                "{error:#}"
            );
        },
    );
}

use super::super::{queued_owner, with_prepared_owner_storage, CanonicalInventory};
use super::{canonical_job, DAY};
use alloy_consensus::Sealable;
use alloy_primitives::U256;
use outbe_metadosis::{
    api::read_live_ocomp_jobs,
    model::{JobFsmCommand, JobFsmState},
    test_support::{seed_ready_worldwide_days_for_capacity, ForkInstallScenario},
    WwdStatus,
};
use outbe_ocomp_protocol::{
    intent::JobIntentV1,
    profile::{poc_schema_limits, ProtocolBundleV1},
    receipts::{desis_request_brief_hash, LimitSplitDestination, RequestLimitSplitReceiptV1},
    state::{OcompJobRecordV1, OcompJobStatus},
};
use outbe_ocompregistry::{OcompProtocolAuthorityV1, OcompRegistry};
use outbe_primitives::{
    addresses::METADOSIS_ADDRESS,
    storage::{
        hashmap::HashMapStorageProvider,
        types::{StorageBytes, StorageKey},
        StorageHandle,
    },
    OutbeHeader,
};

#[derive(Clone, Copy, Debug)]
enum Phase {
    AwaitingFinality,
    VotingOpen,
}

struct ActiveFixture {
    owner: HashMapStorageProvider,
    job: OcompJobRecordV1,
    bundle: ProtocolBundleV1,
}

fn fixture(
    request: &OutbeHeader,
    phase: Phase,
    configure_input: impl FnOnce(&mut JobIntentV1),
) -> ActiveFixture {
    fixture_for_identity(
        request,
        phase,
        1,
        alloy_primitives::B256::repeat_byte(11),
        configure_input,
    )
}

// Test-only variant binding the native integration fixture to its real chain.
fn fixture_for_identity(
    request: &OutbeHeader,
    phase: Phase,
    chain_id: u64,
    genesis_hash: alloy_primitives::B256,
    configure_input: impl FnOnce(&mut JobIntentV1),
) -> ActiveFixture {
    let limits = poc_schema_limits();
    let mut job = canonical_job(request, false);
    job.intent.chain_id = chain_id;
    job.intent.genesis_hash = genesis_hash;
    let install = ForkInstallScenario::final_at(
        outbe_ocompregistry::OCOMP_POC_FINAL_ACTIVATION_HEIGHT,
        job.intent.chain_id,
        job.intent.genesis_hash,
    )
    .unwrap()
    .into_install();
    let profile = &install.request_profile;
    job.intent.fork_id = profile.fork_id;
    job.intent.protocol_bundle_hash = profile.protocol_bundle_hash;
    job.intent.source_availability_policy_id = profile.source_availability_policy_id;
    job.intent_height = request.inner.number;
    job.intent.logical_evaluation_height = request.inner.number;
    job.intent.logical_evaluation_time = request.inner.timestamp;
    configure_input(&mut job.intent);
    assert_eq!(job.intent.wwd, DAY.value());

    let frozen = &job.intent.frozen_metadosis_values;
    let receipt = RequestLimitSplitReceiptV1 {
        protocol_bundle_hash: job.intent.protocol_bundle_hash,
        wwd: job.intent.wwd,
        pending_nonce: 0,
        day_type: frozen.day_type,
        day_limit: frozen.day_limit,
        lysis_limit_minor: frozen.lysis_limit_minor,
        desis_limit_minor: frozen.desis_limit_minor,
        destination: LimitSplitDestination::DesisAuction,
        desis_brief_hash: Some(
            desis_request_brief_hash(
                job.intent.protocol_bundle_hash,
                job.intent.wwd,
                frozen.desis_limit_minor,
                job.intent.logical_evaluation_time,
            )
            .unwrap(),
        ),
        carry_over_credit: U256::ZERO,
        logical_anchor: job.intent.logical_evaluation_time,
    };
    let receipt_hash = receipt.receipt_hash(&limits).unwrap();
    job.intent
        .frozen_metadosis_values
        .request_limit_split_receipt_hash = receipt_hash;
    let intent_id = job.intent.intent_id(&limits).unwrap();
    let request_deadline = job.intent_height.checked_add(64).unwrap();
    // 64 is native OCOMP_AWAITING_FINALITY_DEADLINE_BLOCKS, currently private
    // behind the owner module. Keep this constant confined to the fixture.
    let mut fsm = JobFsmState::initial_ready(DAY, job.intent_height);
    fsm.apply(JobFsmCommand::Request {
        at_height: job.intent_height,
        deadline_height: request_deadline,
        intent_id,
        lysis_limit_minor: receipt.lysis_limit_minor,
        request_limit_receipt_hash: receipt_hash,
    })
    .unwrap();
    match phase {
        Phase::AwaitingFinality => {
            job.status = OcompJobStatus::AwaitingFinality;
            job.finalized = None;
        }
        Phase::VotingOpen => {
            job.status = OcompJobStatus::VotingOpen;
            let finalized = job.finalized.as_mut().unwrap();
            finalized.job_id = job
                .intent
                .job_id(request.hash_slow(), request.inner.state_root, &limits)
                .unwrap();
            finalized.finality_recorded_height = job.intent_height;
            finalized.open_height = job.intent_height.checked_add(4).unwrap();
            finalized.deadline_height = job.intent_height.checked_add(100).unwrap();
            fsm.apply(JobFsmCommand::OpenVoting {
                at_height: finalized.open_height,
                deadline_height: finalized.deadline_height,
            })
            .unwrap();
        }
    }
    job.validate_semantics(&limits).unwrap();

    let mut owner = HashMapStorageProvider::new_with_chain_identity(
        job.intent.chain_id,
        job.intent.genesis_hash,
    );
    owner.set_block_number(install.activation_height);
    StorageHandle::enter(&mut owner, |storage| {
        OcompRegistry::new(storage.clone())
            .initialize_genesis_authority(
                &OcompProtocolAuthorityV1 {
                    request_profile: profile.clone(),
                    protocol_bundle: install.protocol_bundle.clone(),
                },
                install.install_hash(&limits).unwrap(),
                install.activation_height,
                install.activation_height,
                &limits,
            )
            .unwrap();
        seed_ready_worldwide_days_for_capacity(storage, &[DAY]).unwrap();
    });
    // Native schema scalar mappings use DAY.mapping_slot(base+field_offset).
    // The public owner fixture created the Ready aggregate membership.
    let status_slot = DAY.mapping_slot(U256::from(1));
    assert_eq!(
        owner.storage.get(&(METADOSIS_ADDRESS, status_slot)),
        Some(&U256::from(WwdStatus::Ready.as_u8()))
    );
    owner.storage.insert(
        (METADOSIS_ADDRESS, status_slot),
        U256::from(WwdStatus::OffchainPending.as_u8()),
    );
    for (base, value) in [
        (8_u64, receipt.day_limit),
        (9, job.intent.frozen_metadosis_values.previous_vwap),
        (10, job.intent.frozen_metadosis_values.current_vwap),
    ] {
        owner.storage.insert(
            (METADOSIS_ADDRESS, DAY.mapping_slot(U256::from(base))),
            value,
        );
    }

    // Exact bounded persistence of the public model's valid snapshot.
    // Current owner codec.rs: OMJS/v1, pending=2, fixed-width big endian.
    let snapshot = fsm.snapshot();
    let live = snapshot.live.unwrap();
    let mut scheduler = b"OMJS".to_vec();
    scheduler.extend_from_slice(&1_u16.to_be_bytes());
    scheduler.push(2);
    scheduler.extend_from_slice(&snapshot.worldwide_day.value().to_be_bytes());
    scheduler.extend_from_slice(&live.pending_nonce.to_be_bytes());
    scheduler.extend_from_slice(&0_u64.to_be_bytes());
    scheduler.extend_from_slice(live.intent_id.as_slice());
    scheduler.extend_from_slice(&live.requested_height.to_be_bytes());
    scheduler.extend_from_slice(&live.deadline_height.unwrap().to_be_bytes());
    scheduler.push(1);
    scheduler.extend_from_slice(&live.retained_effect.effect_nonce.to_be_bytes());
    scheduler.extend_from_slice(&live.retained_effect.lysis_limit_minor.to_be_bytes::<32>());
    scheduler.extend_from_slice(live.retained_effect.receipt_hash.as_slice());
    assert_eq!(scheduler.len(), 148);
    // OMLI/v2 stores only the WWD/IntentId keys. The FSM is persisted separately.
    let mut live_index = b"OMLI".to_vec();
    live_index.extend_from_slice(&2_u16.to_be_bytes());
    live_index.extend_from_slice(&1_u16.to_be_bytes());
    live_index.extend_from_slice(&snapshot.worldwide_day.value().to_be_bytes());
    live_index.extend_from_slice(live.intent_id.as_slice());
    assert_eq!(live_index.len(), 44);
    StorageHandle::enter(&mut owner, |storage| {
        let write = |slot, bytes: &[u8]| {
            StorageBytes::new(slot, METADOSIS_ADDRESS, storage.clone())
                .write(bytes)
                .unwrap();
        };
        write(U256::from(18), &live_index);
        write(
            DAY.mapping_slot(U256::from(20)),
            &receipt.encode_canonical(&limits).unwrap(),
        );
        write(DAY.mapping_slot(U256::from(23)), &scheduler);
        write(
            outbe_ocomp_protocol::intent::intent_storage_key(intent_id)
                .unwrap()
                .mapping_slot(U256::from(19)),
            &job.encode_canonical(&limits).unwrap(),
        );
        if let Some(finalized) = &job.finalized {
            let mut response = b"OMDI".to_vec();
            response.extend_from_slice(&1_u16.to_be_bytes());
            response.extend_from_slice(&1_u16.to_be_bytes());
            response.extend_from_slice(&finalized.deadline_height.to_be_bytes());
            response.extend_from_slice(finalized.job_id.as_slice());
            response.extend_from_slice(intent_id.as_slice());
            write(U256::from(27), &response);
        }
        assert_eq!(
            read_live_ocomp_jobs(storage).unwrap(),
            vec![(intent_id, job.clone())]
        );
    });
    // Complete the independent empty NOD inventory without overriding owner words.
    for (key, value) in queued_owner(0).storage {
        assert!(owner.storage.insert(key, value).is_none());
    }
    ActiveFixture {
        owner,
        job,
        bundle: install.protocol_bundle,
    }
}

#[test]
fn genuine_active_owner_is_discovered_without_any_local_job_population() {
    for version in [1, 2] {
        for phase in [Phase::AwaitingFinality, Phase::VotingOpen] {
            with_prepared_owner_storage(
                version,
                400,
                |request| {
                    let prepared = fixture(request, phase, |_| {});
                    assert_eq!(
                        prepared
                            .bundle
                            .protocol_bundle_hash(&poc_schema_limits())
                            .unwrap(),
                        prepared.job.intent.protocol_bundle_hash
                    );
                    prepared.owner
                },
                |state, source| {
                    let request = source.header(100).unwrap().unwrap();
                    let expected = fixture(&request, phase, |_| {}).job;
                    let scratch = tempfile::tempdir().unwrap();
                    let inventory =
                        CanonicalInventory::scan(state, scratch.path(), &source.protected, None)
                            .unwrap();
                    assert_eq!(inventory.bounds.active_intents, 1);
                    assert_eq!(
                        inventory.active_jobs(),
                        &[(
                            expected.intent.intent_id(&poc_schema_limits()).unwrap(),
                            expected
                        )]
                    );
                    assert_eq!(inventory.bounds.nod_entries, 0);
                },
            );
        }
    }
}
