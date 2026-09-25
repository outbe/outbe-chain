use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_ocomp_protocol::state::OcompJobRecordV1;

use super::super::{
    native::RethReadOnlyView,
    validation::{
        canonical_state::CanonicalState,
        evm::verify_current_evm,
        headers::{verify_retained_headers, HeaderAudit},
    },
};

#[test]
fn scratch_storage_lookup_requires_the_exact_hashed_slot() {
    for version in [1, 2] {
        let (_source, layout, _) = super::evm::state_fixture(version);
        let scratch = tempfile::tempdir().unwrap();
        let source = RethReadOnlyView::open(&layout).unwrap();
        let verified = verify_current_evm(&source, scratch.path()).unwrap();
        // Storage-only checks need no historical header identities.
        let headers = HeaderAudit {
            intervals: Vec::new(),
            required_missing: Vec::new(),
            verified_headers: 0,
        };
        let state = CanonicalState::new(&verified, &source, &headers);
        let address = Address::repeat_byte(0x11);
        let present = B256::repeat_byte(0x22);
        let absent = (0_u64..1024)
            .map(|number| B256::from(U256::from(number).to_be_bytes::<32>()))
            .find(|slot| keccak256(slot) < keccak256(present))
            .expect("a missing slot preceding the native duplicate");

        assert_eq!(
            state
                .with_storage(|storage| storage.sload(address, U256::from_be_bytes(present.0)))
                .unwrap(),
            U256::from(123)
        );
        assert_eq!(
            state
                .with_storage(|storage| storage.sload(address, U256::from_be_bytes(absent.0)))
                .unwrap(),
            U256::ZERO,
            "seek_by_key_subkey may return the adjacent greater slot"
        );
    }
}

#[test]
fn external_live_job_view_exposes_public_protocol_types() {
    let (_source, layout, _) = super::evm::state_fixture(2);
    let scratch = tempfile::tempdir().unwrap();
    let source = RethReadOnlyView::open(&layout).unwrap();
    let verified = verify_current_evm(&source, scratch.path()).unwrap();
    let headers = HeaderAudit {
        intervals: Vec::new(),
        required_missing: Vec::new(),
        verified_headers: 0,
    };
    let state = CanonicalState::new(&verified, &source, &headers);
    let owner_jobs: Vec<(B256, OcompJobRecordV1)> = state
        .with_storage(outbe_metadosis::api::read_live_ocomp_jobs)
        .unwrap();
    let adapter_jobs: Vec<(B256, OcompJobRecordV1)> = state.live_ocomp_jobs().unwrap();
    assert!(owner_jobs.is_empty());
    assert_eq!(adapter_jobs, owner_jobs);
}

use std::collections::BTreeMap;

use alloy_consensus::Sealable;
use outbe_intex::schema::{CertifiedPayoutRound, IntexContract, SeriesId, SeriesRecord};
use outbe_nod::schema::{NodCertifiedGenerationProjection, NodContract};
use outbe_primitives::{
    storage::{hashmap::HashMapStorageProvider, types::Storable, StorageHandle},
    time::WorldwideDay,
};
use reth_ethereum::{
    provider::db::{
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        tables,
        transaction::{DbTx, DbTxMut},
    },
    trie::root::{state_root_unhashed, storage_root_unhashed},
};
use reth_primitives_traits::{Account, StorageEntry};

use crate::OutbeHeader;

const DAY: WorldwideDay = WorldwideDay::new(20_260_723);

/// Materialize owner-produced storage words in native authoritative tables and
/// recompute header(E) before the production verifier creates immutable scratch.
fn with_owner_storage(
    version: u32,
    owner: HashMapStorageProvider,
    check: impl FnOnce(&CanonicalState<'_>, &RethReadOnlyView),
) {
    with_prepared_owner_storage(version, 101, |_| owner, check);
}

fn with_prepared_owner_storage(
    version: u32,
    execution_height: u64,
    prepare: impl FnOnce(&OutbeHeader) -> HashMapStorageProvider,
    check: impl FnOnce(&CanonicalState<'_>, &RethReadOnlyView),
) {
    let (_source, layout, _) = super::evm::state_fixture(version);
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    let request = tx
        .get::<tables::Headers<OutbeHeader>>(100)
        .unwrap()
        .unwrap();
    let owner = prepare(&request);
    tx.clear::<tables::PlainAccountState>().unwrap();
    tx.clear::<tables::PlainStorageState>().unwrap();
    tx.clear::<tables::HashedAccounts>().unwrap();
    tx.clear::<tables::HashedStorages>().unwrap();
    let mut accounts: BTreeMap<Address, Vec<(B256, U256)>> = BTreeMap::new();
    for ((address, slot), value) in owner.storage {
        if !value.is_zero() {
            accounts
                .entry(address)
                .or_default()
                .push((B256::from(slot.to_be_bytes::<32>()), value));
        }
    }
    let root = state_root_unhashed(accounts.iter().map(|(address, words)| {
        (
            *address,
            Account::default().into_trie_account(storage_root_unhashed(words.iter().copied())),
        )
    }));
    for (address, words) in accounts {
        if version == 1 {
            tx.put::<tables::PlainAccountState>(address, Account::default())
                .unwrap();
            for (key, value) in words {
                tx.put::<tables::PlainStorageState>(address, StorageEntry { key, value })
                    .unwrap();
            }
        } else {
            tx.put::<tables::HashedAccounts>(keccak256(address), Account::default())
                .unwrap();
            for (key, value) in words {
                tx.put::<tables::HashedStorages>(
                    keccak256(address),
                    StorageEntry {
                        key: keccak256(key),
                        value,
                    },
                )
                .unwrap();
            }
        }
    }
    let mut header = tx
        .get::<tables::Headers<OutbeHeader>>(101)
        .unwrap()
        .unwrap();
    header.inner.state_root = root;
    header.inner.timestamp = 1_010;
    header.inner.number = execution_height;
    tx.put::<tables::CanonicalHeaders>(execution_height, header.hash_slow())
        .unwrap();
    tx.put::<tables::Headers<OutbeHeader>>(execution_height, header)
        .unwrap();
    for stage in ["Execution", "Finish"] {
        let mut checkpoint = tx
            .get::<tables::StageCheckpoints>(stage.into())
            .unwrap()
            .unwrap();
        checkpoint.block_number = execution_height;
        tx.put::<tables::StageCheckpoints>(stage.into(), checkpoint)
            .unwrap();
    }
    tx.clear::<tables::AccountChangeSets>().unwrap();
    tx.clear::<tables::StorageChangeSets>().unwrap();
    tx.commit().unwrap();
    drop(db);
    let before = super::headers::fingerprint(_source.path());
    let scratch = tempfile::tempdir().unwrap();
    let source = RethReadOnlyView::open(&layout).unwrap();
    let verified = verify_current_evm(&source, scratch.path()).unwrap();
    let headers = verify_retained_headers(&source, &[99]).unwrap();
    assert_eq!(headers.required_missing, vec![99]);
    let state = CanonicalState::new(&verified, &source, &headers);
    check(&state, &source);
    assert_eq!(super::headers::fingerprint(_source.path()), before);
}

fn seed_nod_generation(
    storage: StorageHandle<'_>,
    day: WorldwideDay,
    sequence: u64,
) -> NodCertifiedGenerationProjection {
    let generation = NodCertifiedGenerationProjection {
        worldwide_day: day,
        generation: sequence,
        job_id: B256::repeat_byte(1),
        program_semantics_hash: B256::repeat_byte(2),
        protocol_bundle_hash: B256::repeat_byte(3),
        nod_root: B256::repeat_byte(4),
        bucket_root: B256::repeat_byte(5),
        output_manifest_root: B256::repeat_byte(6),
        tribute_count: 3,
        nod_count: 3,
        bucket_count: 1,
        nod_amount_total: U256::from(99),
        lysis_allocation_minor: U256::from(77),
        issued_at: 1_000,
        next_nod_ordinal: 1,
        last_progress_height: 90,
    };
    let nod = NodContract::new(storage);
    nod.ocomp_target_generation
        .write(&day, generation.generation)
        .unwrap();
    nod.ocomp_namespace_root
        .write(&day, generation.nod_root)
        .unwrap();
    nod.ocomp_bucket_root
        .write(&day, generation.bucket_root)
        .unwrap();
    nod.ocomp_output_manifest_root
        .write(&day, generation.output_manifest_root)
        .unwrap();
    nod.ocomp_generation_metadata
        .write(&day, generation.metadata_word())
        .unwrap();
    nod.ocomp_nod_amount_total
        .write(&day, generation.nod_amount_total)
        .unwrap();
    nod.ocomp_lysis_allocation_minor
        .write(&day, generation.lysis_allocation_minor)
        .unwrap();
    nod.ocomp_materialization_job_id
        .write(&day, generation.job_id)
        .unwrap();
    nod.ocomp_materialization_protocol_bundle_hash
        .write(&day, generation.protocol_bundle_hash)
        .unwrap();
    nod.ocomp_materialization_program_semantics_hash
        .write(&day, generation.program_semantics_hash)
        .unwrap();
    nod.ocomp_materialization_next_nod_ordinal
        .write(&day, generation.next_nod_ordinal)
        .unwrap();
    nod.ocomp_materialization_last_progress_height
        .write(&day, generation.last_progress_height)
        .unwrap();
    nod.ocomp_materialization_queue_wwd
        .write(&sequence, day)
        .unwrap();
    generation
}

fn series(day: WorldwideDay, currency: [u8; 3]) -> SeriesRecord {
    SeriesRecord {
        series_id: SeriesId::pack(day, currency, b'U').unwrap(),
        issuance_currency: 840,
        reference_currency: 840,
        issued_units: 3,
        promis_load_minor: U256::from(10),
        entry_price_minor: U256::from(11),
        floor_price_minor: U256::from(12),
        call_price_minor: U256::from(13),
        call_window_seconds: 14,
        call_threshold_seconds: 15,
        call_notice_period_seconds: 16,
        issued_at: 1_000,
        called_at: 0,
        state: 0,
        worldwide_day: day,
    }
}

#[test]
fn typed_owner_reads_enumerate_all_nod_sequences_and_intex_words_at_e() {
    for version in [1, 2] {
        let mut owner = HashMapStorageProvider::new(1);
        let days = [DAY, WorldwideDay::new(DAY.value() + 1)];
        let records = [series(days[0], *b"USD"), series(days[1], *b"EUR")];
        let rounds = days.map(|day| CertifiedPayoutRound {
            wwd: day.value(),
            amount: U256::from(100),
            paid_so_far: U256::from(20),
            paid_leaf_count: 1,
            active: 1,
        });
        let generations = StorageHandle::enter(&mut owner, |storage| {
            let nod = NodContract::new(storage.clone());
            nod.ocomp_materialization_head_sequence.write(7).unwrap();
            nod.ocomp_materialization_tail_sequence.write(9).unwrap();
            let generations = [
                seed_nod_generation(storage.clone(), days[0], 7),
                seed_nod_generation(storage.clone(), days[1], 8),
            ];
            let intex = IntexContract::new(storage);
            intex.total_series.write(2).unwrap();
            for (index, record) in records.iter().enumerate() {
                let day = record.worldwide_day;
                intex.series.create(record).unwrap();
                intex
                    .series_id_at_index
                    .write(&(index as u64), record.series_id.to_word())
                    .unwrap();
                intex
                    .ocomp_contributor_root
                    .write(&day, B256::repeat_byte(9))
                    .unwrap();
                intex
                    .ocomp_contributor_metadata
                    .write(&day, U256::ONE | (U256::from(2) << 64))
                    .unwrap();
                intex
                    .ocomp_eligible_nominal_total
                    .write(&day, U256::from(50))
                    .unwrap();
                intex.ocomp_payout_round.create(&rounds[index]).unwrap();
                for word in [0_u32, 1] {
                    intex
                        .ocomp_paid_leaves
                        .write(
                            &IntexContract::paid_bitmap_key(day.value(), word),
                            U256::from(3 + word),
                        )
                        .unwrap();
                }
            }
            generations
        });
        with_owner_storage(version, owner, |state, _| {
            assert_eq!(state.nod_materialization_bounds().unwrap(), (7, 9));
            let head = state.nod_materialization_head().unwrap().unwrap();
            assert_eq!(
                (head.queue_sequence, head.worldwide_day),
                (7, days[0].value())
            );
            assert_eq!(state.intex_total_series().unwrap(), 2);
            for (index, day) in days.into_iter().enumerate() {
                assert_eq!(
                    state.nod_materialization_day(7 + index as u64).unwrap(),
                    day
                );
                assert_eq!(
                    state.nod_certified_generation(day).unwrap(),
                    Some(generations[index])
                );
                assert_eq!(
                    state.intex_series_id_at(index as u64).unwrap(),
                    records[index].series_id
                );
                assert_eq!(
                    state.intex_read_series(records[index].series_id).unwrap(),
                    records[index]
                );
                let certified = state
                    .intex_certified_contributor_generation(day)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    (
                        certified.worldwide_day,
                        certified.series_version,
                        certified.contributor_count
                    ),
                    (day.value(), 1, 2)
                );
                assert_eq!(certified.contributor_root, B256::repeat_byte(9));
                assert_eq!(certified.eligible_nominal_total, U256::from(50));
                assert_eq!(
                    state.intex_certified_payout_round(day.value()).unwrap(),
                    Some(rounds[index].clone())
                );
                for word in [0_u32, 1] {
                    assert_eq!(
                        state.intex_paid_leaves_word(day.value(), word).unwrap(),
                        U256::from(3 + word)
                    );
                }
            }
        });
    }
}

#[test]
fn absent_owner_generations_remain_optional() {
    let mut owner = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut owner, |storage| {
        let nod = NodContract::new(storage);
        nod.ocomp_materialization_head_sequence.write(1).unwrap();
        nod.ocomp_materialization_tail_sequence.write(1).unwrap();
    });
    with_owner_storage(2, owner, |state, _| {
        assert_eq!(state.nod_materialization_bounds().unwrap(), (1, 1));
        assert!(state.nod_materialization_head().unwrap().is_none());
        assert!(state.nod_certified_generation(DAY).unwrap().is_none());
        assert!(state
            .intex_certified_contributor_generation(DAY)
            .unwrap()
            .is_none());
        assert!(state
            .intex_certified_payout_round(DAY.value())
            .unwrap()
            .is_none());
        assert!(state
            .metadosis_active_lysis_generation(DAY)
            .unwrap()
            .is_none());
    });
}

#[test]
fn residual_intex_generation_and_malformed_nod_head_keep_owner_errors() {
    let mut owner = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut owner, |storage| {
        IntexContract::new(storage.clone())
            .ocomp_contributor_root
            .write(&DAY, B256::repeat_byte(9))
            .unwrap();
        let nod = NodContract::new(storage);
        nod.ocomp_materialization_head_sequence.write(7).unwrap();
        nod.ocomp_materialization_tail_sequence.write(8).unwrap();
        nod.ocomp_materialization_queue_wwd.write(&7, DAY).unwrap();
    });
    with_owner_storage(2, owner, |state, _| {
        assert!(state.intex_certified_contributor_generation(DAY).is_err());
        assert!(state.nod_materialization_head().is_err());
    });
}

#[test]
fn intex_series_index_rejects_nonzero_padding_lost_by_native_decode() {
    let mut owner = HashMapStorageProvider::new(1);
    let id = series(DAY, *b"USD").series_id;
    StorageHandle::enter(&mut owner, |storage| {
        let intex = IntexContract::new(storage);
        intex.total_series.write(1).unwrap();
        intex
            .series_id_at_index
            .write(&0, id.to_word() | U256::ONE)
            .unwrap();
        assert_eq!(
            outbe_intex::api::series_id_at(&intex.storage, 0).unwrap(),
            id
        );
    });
    with_owner_storage(2, owner, |state, _| {
        assert!(state.intex_series_id_at(0).is_err());
    });
}

#[test]
fn storage_context_is_current_e_and_hashes_are_limited_to_audited_history() {
    with_owner_storage(2, HashMapStorageProvider::new(1), |state, source| {
        let context = state
            .with_storage(|storage| {
                Ok((
                    storage.chain_id()?,
                    storage.genesis_hash()?,
                    storage.block_number()?,
                    storage.timestamp()?,
                ))
            })
            .unwrap();
        assert_eq!(
            context,
            (
                source.chain.chain().id(),
                source.chain.genesis_hash(),
                101,
                U256::from(1_010)
            )
        );
        for number in [0, 100, 101] {
            let hash = state
                .with_storage(|storage| storage.canonical_block_hash(number))
                .unwrap();
            assert_eq!(
                hash,
                Some(source.header(number).unwrap().unwrap().hash_slow())
            );
        }
        for number in [99, 102] {
            assert!(state
                .with_storage(|storage| storage.canonical_block_hash(number))
                .unwrap()
                .is_none());
        }
        assert!(state
            .with_storage(|storage| storage.sstore(Address::ZERO, U256::ZERO, U256::ONE))
            .is_err());
    });
}

use outbe_ocomp_protocol::{
    hash::hash_framed,
    intent::{
        intent_storage_key, ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType,
        FrozenMetadosisValuesV1, JobIntentV1, MetadosisAttemptPreconditionV1,
        MetadosisExpectedStatus, NodTargetPreconditionV1, TributeInputBindingV1,
    },
    profile::poc_schema_limits,
    receipts::{ActivationOutcome, AggregateActivationReceiptV1, EffectBindingV1},
    registry::HashDomain,
    state::{
        LysisTerminalV1, OcompCompletedBindingV1, OcompFinalizedJobV1, OcompJobStatus,
        OcompTerminalOutcome,
    },
    vote::OcompQuorumV1,
};
use outbe_primitives::{
    addresses::METADOSIS_ADDRESS,
    storage::types::{StorageBytes, StorageKey},
};

fn canonical_job(request: &OutbeHeader, completed: bool) -> OcompJobRecordV1 {
    let hash = B256::repeat_byte(11);
    let intent = JobIntentV1 {
        chain_id: 1,
        genesis_hash: hash,
        fork_id: hash,
        wwd: DAY.value(),
        pending_nonce: 0,
        attempt: 0,
        protocol_bundle_hash: hash,
        ce_sealed_root: hash,
        sealed_tribute_collection_key: hash,
        sealed_tribute_collection_root: hash,
        authenticated_day_count: 1,
        authenticated_day_nominal: U256::from(10),
        pre_admission_envelope_hash: hash,
        source_availability_policy_id: hash,
        frozen_metadosis_values: FrozenMetadosisValuesV1 {
            day_type: DayType::Green,
            day_limit: U256::from(10),
            previous_vwap: U256::ONE,
            current_vwap: U256::ONE,
            gratis_demand: U256::from(10),
            day_gratis_limit_minor: U256::from(10),
            lysis_limit_minor: U256::from(10),
            desis_limit_minor: U256::ZERO,
            auction_entry_prices: Vec::new(),
            request_limit_split_receipt_hash: hash,
        },
        logical_evaluation_height: 90,
        logical_evaluation_time: 900,
        activation_preconditions: ActivationPreconditionsV1 {
            tribute: TributeInputBindingV1 {
                wwd: DAY.value(),
                source_generation: 0,
                collection_key: hash,
                sealed_collection_root: hash,
                exact_count: 1,
                exact_nominal_total: U256::from(10),
            },
            nod: NodTargetPreconditionV1 {
                wwd: DAY.value(),
                target_generation: 0,
                namespace_root_before: B256::ZERO,
                max_nod_count: 1,
            },
            contributors: ContributorTargetPreconditionV1 {
                worldwide_day: DAY.value(),
                expected_series_version: 0,
                max_contributor_count: 1,
                max_eligible_nominal_total: U256::from(10),
            },
            metadosis: MetadosisAttemptPreconditionV1 {
                wwd: DAY.value(),
                pending_nonce: 0,
                expected_status: MetadosisExpectedStatus::OffchainPending,
                state_version: 2,
            },
        },
        result_validator_set_epoch: 1,
        result_committee_set_hash: hash,
        result_ocomp_binding_hash: hash,
        result_member_count: 4,
        result_quorum_threshold: 3,
        custody_committee_epoch_hash: None,
    };
    let limits = poc_schema_limits();
    let intent_id = intent.intent_id(&limits).unwrap();
    let job_id = intent
        .job_id(request.hash_slow(), request.inner.state_root, &limits)
        .unwrap();
    let mut finalized = OcompFinalizedJobV1 {
        job_id,
        finalized_request_block_hash: request.hash_slow(),
        finalized_request_state_root: request.inner.state_root,
        finality_recorded_height: 100,
        open_height: 104,
        deadline_height: 200,
        quorum: None,
    };
    let terminal = if completed {
        let quorum = OcompQuorumV1 {
            member_count: 4,
            quorum_threshold: 3,
            result_digest: hash,
            quorum_height: 105,
            signer_bitmap: vec![7],
            evidence_hash: hash,
        };
        let receipt = AggregateActivationReceiptV1 {
            binding: EffectBindingV1 {
                intent_id,
                job_id,
                attempt: 0,
                protocol_bundle_hash: hash,
                result_digest: hash,
                activation_preconditions_hash: intent
                    .activation_preconditions
                    .activation_preconditions_hash(&limits)
                    .unwrap(),
                activation_call_id: hash,
            },
            outcome: ActivationOutcome::Applied,
            nod_receipt_hash: Some(hash),
            contributor_receipt_hash: Some(hash),
            tribute_receipt_hash: Some(hash),
            carry_over_receipt_hash: Some(hash),
            request_limit_split_receipt_hash: hash,
            active_generation_hash: Some(hash),
            effect_commitment: hash_framed(HashDomain::Effects, &hash.as_slice().repeat(4))
                .unwrap(),
            event_summary_hash: hash,
            activated_at_height: 105,
            activated_at_time: 1_005,
        };
        let binding = OcompCompletedBindingV1 {
            job_id,
            activation_call_id: hash,
            result_digest: hash,
            quorum_height: quorum.quorum_height,
            quorum_signer_bitmap: quorum.signer_bitmap.clone(),
            quorum_evidence_hash: quorum.evidence_hash,
            result_evidence_hash: hash,
            terminal_receipt_hash: receipt.terminal_receipt_hash(&limits).unwrap(),
            terminal_receipt: receipt,
        };
        finalized.quorum = Some(quorum);
        Some(LysisTerminalV1 {
            outcome: OcompTerminalOutcome::Completed,
            terminal_height: 105,
            terminal_time: 1_005,
            completed_binding: Some(binding),
        })
    } else {
        None
    };
    let record = OcompJobRecordV1 {
        intent,
        intent_height: 100,
        status: if completed {
            OcompJobStatus::Completed
        } else {
            OcompJobStatus::AwaitingFinality
        },
        finalized: Some(finalized),
        terminal,
    };
    record.validate_semantics(&limits).unwrap();
    record
}

fn stored_job(intent_id: B256, encoded: &[u8]) -> HashMapStorageProvider {
    let mut owner = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut owner, |storage| {
        // Fixed OCM proof mapping base. The snapshot adapter itself never reads
        // Metadosis slots directly or imports its private schema.
        let slot = intent_storage_key(intent_id)
            .unwrap()
            .mapping_slot(U256::from(20));
        StorageBytes::new(slot, METADOSIS_ADDRESS, storage)
            .write(encoded)
            .unwrap();
    });
    owner
}

#[test]
fn current_job_and_terminal_receipt_bind_to_retained_request_without_changesets() {
    for version in [1, 2] {
        for completed in [false, true] {
            with_prepared_owner_storage(
                version,
                110,
                |request| {
                    let record = canonical_job(request, completed);
                    stored_job(
                        record.intent.intent_id(&poc_schema_limits()).unwrap(),
                        &record.encode_canonical(&poc_schema_limits()).unwrap(),
                    )
                },
                |state, source| {
                    let expected = canonical_job(&source.header(100).unwrap().unwrap(), completed);
                    let intent_id = expected.intent.intent_id(&poc_schema_limits()).unwrap();
                    let job_id = expected.finalized.as_ref().unwrap().job_id;
                    assert_eq!(
                        state.metadosis_job(intent_id, DAY, Some(job_id)).unwrap(),
                        expected
                    );
                    assert_eq!(
                        state
                            .with_storage(|storage| storage.block_number())
                            .unwrap(),
                        110
                    );
                    assert_eq!(
                        state
                            .with_storage(|storage| storage.canonical_block_hash(100))
                            .unwrap(),
                        Some(
                            expected
                                .finalized
                                .as_ref()
                                .unwrap()
                                .finalized_request_block_hash
                        )
                    );
                    if completed {
                        let receipt = expected
                            .terminal
                            .as_ref()
                            .unwrap()
                            .completed_binding
                            .as_ref()
                            .unwrap()
                            .terminal_receipt
                            .clone();
                        assert_eq!(
                            state
                                .metadosis_terminal_receipt(intent_id, DAY, job_id)
                                .unwrap(),
                            receipt
                        );
                    } else {
                        assert!(state
                            .metadosis_terminal_receipt(intent_id, DAY, job_id)
                            .is_err());
                    }
                    assert!(state
                        .metadosis_job(intent_id, WorldwideDay::new(DAY.value() + 1), Some(job_id))
                        .is_err());
                    assert!(state
                        .metadosis_job(intent_id, DAY, Some(B256::repeat_byte(99)))
                        .is_err());
                    assert!(state
                        .metadosis_terminal_receipt(intent_id, DAY, B256::repeat_byte(99))
                        .is_err());
                },
            );
        }
    }
}

#[test]
fn typed_job_rejects_wrong_stored_intent_and_malformed_ocb1() {
    for malformed in [false, true] {
        with_prepared_owner_storage(
            2,
            110,
            |request| {
                let record = canonical_job(request, false);
                let bytes = if malformed {
                    b"not OCB1".to_vec()
                } else {
                    record.encode_canonical(&poc_schema_limits()).unwrap()
                };
                stored_job(B256::repeat_byte(99), &bytes)
            },
            |state, _| {
                assert!(state
                    .metadosis_job(B256::repeat_byte(99), DAY, None)
                    .is_err());
            },
        );
    }
}

#[test]
fn missing_request_header_is_incomplete_and_mismatched_binding_is_invalid() {
    for corruption in ["missing", "hash", "root"] {
        with_prepared_owner_storage(
            2,
            110,
            |request| {
                let mut record = canonical_job(request, false);
                let finalized = record.finalized.as_mut().unwrap();
                match corruption {
                    "missing" => record.intent_height = 99,
                    "hash" => finalized.finalized_request_block_hash = B256::repeat_byte(99),
                    "root" => finalized.finalized_request_state_root = B256::repeat_byte(99),
                    _ => unreachable!(),
                }
                finalized.job_id = record
                    .intent
                    .job_id(
                        finalized.finalized_request_block_hash,
                        finalized.finalized_request_state_root,
                        &poc_schema_limits(),
                    )
                    .unwrap();
                stored_job(
                    record.intent.intent_id(&poc_schema_limits()).unwrap(),
                    &record.encode_canonical(&poc_schema_limits()).unwrap(),
                )
            },
            |state, source| {
                let record = canonical_job(&source.header(100).unwrap().unwrap(), false);
                let intent_id = record.intent.intent_id(&poc_schema_limits()).unwrap();
                let error = state.metadosis_job(intent_id, DAY, None).unwrap_err();
                assert_eq!(
                    error
                        .downcast_ref::<super::super::validation::Incomplete>()
                        .is_some(),
                    corruption == "missing",
                    "{error:#}"
                );
            },
        );
    }
}

#[test]
fn terminal_receipt_rejects_cross_intent_and_cross_job_bindings() {
    for wrong_job in [false, true] {
        with_prepared_owner_storage(
            2,
            110,
            |request| {
                let mut record = canonical_job(request, true);
                let binding = record
                    .terminal
                    .as_mut()
                    .unwrap()
                    .completed_binding
                    .as_mut()
                    .unwrap();
                if wrong_job {
                    binding.job_id = B256::repeat_byte(99);
                    binding.terminal_receipt.binding.job_id = binding.job_id;
                } else {
                    binding.terminal_receipt.binding.intent_id = B256::repeat_byte(99);
                }
                binding.terminal_receipt_hash = binding
                    .terminal_receipt
                    .terminal_receipt_hash(&poc_schema_limits())
                    .unwrap();
                stored_job(
                    record.intent.intent_id(&poc_schema_limits()).unwrap(),
                    &record.encode_canonical(&poc_schema_limits()).unwrap(),
                )
            },
            |state, source| {
                let record = canonical_job(&source.header(100).unwrap().unwrap(), true);
                let intent_id = record.intent.intent_id(&poc_schema_limits()).unwrap();
                assert!(state
                    .metadosis_terminal_receipt(intent_id, DAY, record.finalized.unwrap().job_id)
                    .is_err());
            },
        );
    }
}

#[test]
fn active_generation_decodes_present_record_and_rejects_malformed_bytes() {
    use std::{cell::Cell, rc::Rc};

    use outbe_ocomp_protocol::{result::ExactCountsV1, state::ActiveGenerationV1};
    use outbe_primitives::{
        error::{PrecompileError, Result},
        storage::readonly::{ReadOnlyStorageProvider, StorageReader},
    };

    struct AbsentGenerationReader(Rc<Cell<Option<U256>>>);

    impl StorageReader for AbsentGenerationReader {
        fn read_storage(&self, address: Address, key: B256) -> Result<U256> {
            assert_eq!(address, METADOSIS_ADDRESS);
            assert!(self.0.replace(Some(U256::from_be_bytes(key.0))).is_none());
            Ok(U256::ZERO)
        }
    }

    // Discover the one native length slot through the public owner getter;
    // this fixture does not import or replicate the private Metadosis schema.
    let observed_slot = Rc::new(Cell::new(None));
    let mut reader = ReadOnlyStorageProvider::new(AbsentGenerationReader(observed_slot.clone()));
    let absent =
        outbe_metadosis::api::get_active_lysis_generation(StorageHandle::new(&mut reader), DAY);
    assert!(
        matches!(absent, Err(PrecompileError::Revert(message)) if message == "ActiveGenerationV1 not found")
    );
    let slot = observed_slot.get().unwrap();
    let expected = ActiveGenerationV1 {
        job_id: B256::repeat_byte(1),
        program_semantics_hash: B256::repeat_byte(2),
        nod_root: B256::repeat_byte(3),
        bucket_root: B256::repeat_byte(4),
        contributor_root: B256::repeat_byte(5),
        output_manifest_root: B256::repeat_byte(6),
        exact_counts: ExactCountsV1 {
            tribute_count: 2,
            nod_count: 2,
            bucket_count: 1,
            contributor_count: 1,
            semantic_event_count: 3,
        },
        result_evidence_hash: B256::repeat_byte(7),
        availability_certificate_hash: None,
    };
    for version in [1, 2] {
        for malformed in [false, true] {
            let mut encoded = expected.encode_canonical(&poc_schema_limits()).unwrap();
            if malformed {
                encoded[0] ^= 1;
            }
            let mut owner = HashMapStorageProvider::new(1);
            StorageHandle::enter(&mut owner, |storage| {
                StorageBytes::new(slot, METADOSIS_ADDRESS, storage)
                    .write(&encoded)
                    .unwrap();
            });
            with_owner_storage(version, owner, |state, _| {
                let actual = state.metadosis_active_lysis_generation(DAY);
                if malformed {
                    assert!(
                        actual.is_err(),
                        "present malformed bytes must not become absent"
                    );
                } else {
                    assert_eq!(actual.unwrap(), Some(expected.clone()));
                }
            });
        }
    }
}
